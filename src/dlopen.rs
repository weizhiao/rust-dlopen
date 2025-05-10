use crate::{
    OpenFlags, Result, find_lib_error,
    loader::{Builder, ElfLibrary, FileBuilder, create_lazy_scope, deal_unknown},
    register::{DylibState, MANAGER, register},
};
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::ffi::{c_char, c_int, c_void};
use elf_loader::{
    ElfDylib, RelocatedDylib,
    mmap::{Mmap, MmapImpl},
};
use spin::Lazy;

#[derive(Debug, PartialEq, Eq, Hash)]
struct ElfPath {
    path: String,
}

impl ElfPath {
    fn from_str(path: &str) -> Result<Self> {
        Ok(ElfPath {
            path: path.to_owned(),
        })
    }

    fn join(&self, file_name: &str) -> ElfPath {
        let mut new = self.path.clone();
        new.push('/');
        new.push_str(file_name);
        ElfPath { path: new }
    }

    fn as_str(&self) -> &str {
        &self.path
    }
}

impl ElfLibrary {
    /// Load a shared library from a specified path. It is the same as dlopen.
    ///
    /// # Example
    /// ```no_run
    /// # use std::path::Path;
    /// # use dlopen_rs::{ElfLibrary, OpenFlags};
    ///
    /// let path = Path::new("/path/to/library.so");
    /// let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_LOCAL).expect("Failed to load library");
    /// ```
    #[cfg(feature = "std")]
    #[inline]
    pub fn dlopen(path: impl AsRef<std::ffi::OsStr>, flags: OpenFlags) -> Result<ElfLibrary> {
        dlopen_impl::<FileBuilder, MmapImpl>(path.as_ref().to_str().unwrap(), flags, || {
            ElfLibrary::from_file(path.as_ref(), flags)
        })
    }

    #[inline]
    pub fn dlopen_from_builder<B, M>(
        path: &str,
        bytes: Option<&[u8]>,
        flags: OpenFlags,
    ) -> Result<ElfLibrary>
    where
        B: Builder,
        M: Mmap,
    {
        if let Some(bytes) = bytes {
            dlopen_impl::<B, M>(path, flags, || ElfLibrary::from_binary(bytes, path, flags))
        } else {
            dlopen_impl::<B, M>(path, flags, || {
                ElfLibrary::from_builder::<B, M>(path, flags)
            })
        }
    }

    /// Load a shared library from bytes. It is the same as dlopen. However, it can also be used in the no_std environment,
    /// and it will look for dependent libraries in those manually opened dynamic libraries.
    #[inline]
    pub fn dlopen_from_binary(
        bytes: &[u8],
        path: impl AsRef<str>,
        flags: OpenFlags,
    ) -> Result<ElfLibrary> {
        dlopen_impl::<FileBuilder, MmapImpl>(path.as_ref(), flags, || {
            ElfLibrary::from_binary(bytes, path.as_ref(), flags)
        })
    }
}

struct Recycler {
    is_recycler: bool,
    old_all_len: usize,
    old_global_len: usize,
}

impl Drop for Recycler {
    fn drop(&mut self) {
        if self.is_recycler {
            log::debug!("Destroying newly added dynamic libraries from the global");
            let mut lock = MANAGER.write();
            lock.all.truncate(self.old_all_len);
            lock.global.truncate(self.old_global_len);
        }
    }
}

fn dlopen_impl<B, M>(
    path: &str,
    flags: OpenFlags,
    f: impl Fn() -> Result<ElfDylib>,
) -> Result<ElfLibrary>
where
    B: Builder,
    M: Mmap,
{
    let shortname = path.split('/').next_back().unwrap();
    log::info!("dlopen: Try to open [{}] with [{:?}] ", path, flags);
    let mut lock = MANAGER.write();
    // 新加载的动态库
    let mut new_libs = Vec::new();
    let core = if flags.contains(OpenFlags::CUSTOM_NOT_REGISTER) {
        let lib = f()?;
        let core = lib.core_component();
        new_libs.push(Some(lib));
        unsafe { RelocatedDylib::from_core_component(core) }
    } else {
        // 检查是否是已经加载的库
        if let Some(lib) = lock.all.get(shortname) {
            if lib.deps().is_some()
                && !flags
                    .difference(lib.flags())
                    .contains(OpenFlags::RTLD_GLOBAL)
            {
                return Ok(lib.get_dylib());
            }
            lib.relocated_dylib()
        } else {
            let lib = f()?;
            let core = lib.core_component();
            new_libs.push(Some(lib));
            unsafe { RelocatedDylib::from_core_component(core) }
        }
    };

    let mut recycler = Recycler {
        is_recycler: true,
        old_all_len: usize::MAX,
        old_global_len: usize::MAX,
    };

    // 用于保存所有的依赖库
    let mut dep_libs = Vec::new();
    let mut cur_pos = 0;
    dep_libs.push(core.clone());
    recycler.old_all_len = lock.all.len();
    recycler.old_global_len = lock.global.len();

    let mut cur_newlib_pos = 0;
    // 广度优先搜索，这是规范的要求，这个循环里会加载所有需要的动态库，无论是直接依赖还是间接依赖的
    while cur_pos < dep_libs.len() {
        let lib_names: &[&str] = unsafe { core::mem::transmute(dep_libs[cur_pos].needed_libs()) };
        let mut cur_rpath = None;
        for lib_name in lib_names {
            if let Some(lib) = lock.all.get_mut(*lib_name) {
                if !lib.state.is_used() {
                    lib.state.set_used();
                    dep_libs.push(lib.relocated_dylib());
                    log::debug!("Use an existing dylib: [{}]", lib.shortname());
                    if flags
                        .difference(lib.flags())
                        .contains(OpenFlags::RTLD_GLOBAL)
                    {
                        let shortname = lib.shortname().to_owned();
                        log::debug!(
                            "Trying to update a library. Name: [{}] Old flags:[{:?}] New flags:[{:?}]",
                            shortname,
                            lib.flags(),
                            flags
                        );
                        lib.set_flags(flags);
                        let core = lib.relocated_dylib();
                        lock.global.insert(shortname, core);
                    }
                }
                continue;
            }

            let rpath = if let Some(rpath) = &cur_rpath {
                rpath
            } else {
                let parent_lib = new_libs[cur_newlib_pos].as_ref().unwrap();
                cur_rpath = Some(
                    parent_lib
                        .rpath()
                        .map(|rpath| fixup_rpath(parent_lib.name(), rpath))
                        .unwrap_or(Box::new([])),
                );
                cur_newlib_pos += 1;
                unsafe { cur_rpath.as_ref().unwrap_unchecked() }
            };

            find_library(rpath, lib_name, |path| {
                let new_lib = ElfLibrary::from_builder::<B, M>(path.as_str(), flags)?;
                let inner = new_lib.core_component();
                register(
                    unsafe { RelocatedDylib::from_core_component(inner.clone()) },
                    flags,
                    None,
                    &mut lock,
                    *DylibState::default()
                        .set_used()
                        .set_new_idx(new_libs.len() as _),
                );
                dep_libs.push(unsafe { RelocatedDylib::from_core_component(inner) });
                new_libs.push(Some(new_lib));
                Ok(())
            })?;
        }
        cur_pos += 1;
    }

    #[derive(Clone, Copy)]
    struct Item {
        idx: usize,
        next: usize,
    }
    // 保存new_libs的索引
    let mut stack = Vec::new();
    stack.push(Item { idx: 0, next: 0 });
    // 记录新加载的动态库进行重定位的顺序
    let mut order = Vec::new();

    'start: while let Some(mut item) = stack.pop() {
        let names = new_libs[item.idx].as_ref().unwrap().needed_libs();
        for name in names.iter().skip(item.next) {
            let lib = lock.all.get_mut(*name).unwrap();
            lib.state.set_unused();
            // 判断当前依赖库是否是新加载的，如果不是则跳过本轮操作，因为它已经被重定位过了
            let Some(idx) = lib.state.get_new_idx() else {
                continue;
            };
            // 将当前依赖库的状态设置为已经重定位
            lib.state.set_relocated();
            item.next += 1;
            stack.push(item);
            stack.push(Item {
                idx: idx as usize,
                next: 0,
            });
            continue 'start;
        }
        order.push(item.idx);
    }

    let deps = Arc::new(dep_libs.into_boxed_slice());
    let core = deps[0].clone();
    let res = ElfLibrary {
        inner: core.clone(),
        flags,
        deps: Some(deps.clone()),
    };
    //重新注册因为更新了deps
    register(
        core,
        flags,
        Some(deps.clone()),
        &mut lock,
        *DylibState::default().set_relocated(),
    );
    let read_lock = lock.downgrade();
    let lazy_scope = create_lazy_scope(&deps);
    let iter: Vec<&RelocatedDylib<'_>> = read_lock.global.values().chain(deps.iter()).collect();
    for idx in order {
        let lib = core::mem::take(&mut new_libs[idx]).unwrap();
        log::debug!("Relocating dylib [{}]", lib.name());
        let is_lazy = lib.is_lazy();
        lib.relocate(
            &iter,
            &|_| None,
            &mut deal_unknown,
            if is_lazy {
                Some(lazy_scope.clone())
            } else {
                None
            },
        )?;
    }
    if !flags.contains(OpenFlags::CUSTOM_NOT_REGISTER) {
        recycler.is_recycler = false;
    }
    Ok(res)
}

static LD_LIBRARY_PATH: Lazy<Box<[ElfPath]>> = Lazy::new(|| {
    #[cfg(feature = "std")]
    {
        let library_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        deal_path(&library_path)
    }
    #[cfg(not(feature = "std"))]
    Box::new([])
});
static DEFAULT_PATH: spin::Lazy<Box<[ElfPath]>> = Lazy::new(|| unsafe {
    let v = vec![
        ElfPath::from_str("/usr/lib").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib").unwrap_unchecked(),
    ];
    v.into_boxed_slice()
});
static LD_CACHE: Lazy<Box<[ElfPath]>> = Lazy::new(build_ld_cache);

#[inline]
fn fixup_rpath(lib_path: &str, rpath: &str) -> Box<[ElfPath]> {
    if !rpath.contains('$') {
        return deal_path(rpath);
    }
    for s in rpath.split('$').skip(1) {
        if !s.starts_with("ORIGIN") && !s.starts_with("{ORIGIN}") {
            log::warn!("DT_RUNPATH format is incorrect: [{}]", rpath);
            return Box::new([]);
        }
    }
    let dir = if let Some((path, _)) = lib_path.rsplit_once('/') {
        path
    } else {
        "."
    };
    deal_path(&rpath.to_string().replace("$ORIGIN", dir))
}

#[inline]
fn deal_path(s: &str) -> Box<[ElfPath]> {
    s.split(":")
        .map(|str| ElfPath::from_str(str).unwrap())
        .collect()
}

#[inline]
fn find_library(
    cur_rpath: &[ElfPath],
    lib_name: &str,
    mut f: impl FnMut(&ElfPath) -> Result<()>,
) -> Result<()> {
    // Search order: DT_RPATH(deprecated) -> LD_LIBRARY_PATH -> DT_RUNPATH -> /etc/ld.so.cache -> /lib:/usr/lib.
    let search_paths = LD_LIBRARY_PATH
        .iter()
        .chain(cur_rpath.iter())
        .chain(LD_CACHE.iter())
        .chain(DEFAULT_PATH.iter());

    for path in search_paths {
        let file_path = path.join(lib_name);
        log::trace!("Try to open dependency shared object: [{:?}]", file_path);
        if f(&file_path).is_ok() {
            return Ok(());
        }
    }
    Err(find_lib_error(format!("can not find file: {}", lib_name)))
}

#[cfg(feature = "std")]
mod imp {
    use super::ElfPath;
    use dynamic_loader_cache::{Cache as LdCache, Result as LdResult};

    #[inline]
    pub(super) fn build_ld_cache() -> Box<[ElfPath]> {
        use std::collections::HashSet;
        LdCache::load()
            .and_then(|cache| {
                Ok(Vec::from_iter(
                    cache
                        .iter()?
                        .filter_map(LdResult::ok)
                        .map(|entry| {
                            // Since the `full_path` is always a file, we can always unwrap it
                            ElfPath::from_str(
                                entry
                                    .full_path
                                    .parent()
                                    .unwrap()
                                    .to_owned()
                                    .to_str()
                                    .unwrap(),
                            )
                            .unwrap()
                        })
                        .collect::<HashSet<_>>(),
                )
                .into_boxed_slice())
            })
            .unwrap_or_else(|err| {
                log::warn!("Build ld cache failed: {}", err);
                Box::new([])
            })
    }
}

#[cfg(not(feature = "std"))]
mod imp {
    use alloc::boxed::Box;

    use super::ElfPath;
    #[inline]
    pub(super) fn build_ld_cache() -> Box<[ElfPath]> {
        Box::new([])
    }
}

use imp::build_ld_cache;

/// # Safety
/// It is the same as `dlopen`.
#[allow(unused_variables)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *const c_void {
    let mut lib = if filename.is_null() {
        MANAGER.read().all.get_index(0).unwrap().1.get_dylib()
    } else {
        #[cfg(feature = "std")]
        {
            let flags = OpenFlags::from_bits_retain(flags as _);
            let filename = unsafe { core::ffi::CStr::from_ptr(filename) };
            let path = filename.to_str().unwrap();
            if let Ok(lib) = ElfLibrary::dlopen(path, flags) {
                lib
            } else {
                return core::ptr::null();
            }
        }
        #[cfg(not(feature = "std"))]
        return core::ptr::null();
    };
    Arc::into_raw(core::mem::take(&mut lib.deps).unwrap()) as _
}
