use crate::core_impl::loader::{ElfDylib, RelocatedDylib};
use crate::{
    OpenFlags, Result,
    core_impl::loader::{DylibExt, ElfLibrary, create_lazy_scope},
    core_impl::register::{DylibState, MANAGER, Manager, register},
    error::find_lib_error,
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
use spin::{Lazy, RwLockWriteGuard};

#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct ElfPath {
    path: String,
}

impl ElfPath {
    pub(crate) fn from_str(path: &str) -> Result<Self> {
        Ok(ElfPath {
            path: path.to_owned(),
        })
    }

    /// Appends a file name to the path, ensuring a separator is present.
    fn join(&self, file_name: &str) -> ElfPath {
        let mut new = self.path.clone();
        if !new.is_empty() && !new.ends_with('/') {
            new.push('/');
        }
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
    /// # use dlopen_rs::{ElfLibrary, OpenFlags};
    ///
    /// let path = "/path/to/library.so";
    /// let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_LOCAL).expect("Failed to load library");
    /// ```
    #[cfg(feature = "fs")]
    #[inline]
    pub fn dlopen(path: impl AsRef<str>, flags: OpenFlags) -> Result<ElfLibrary> {
        dlopen_impl(path.as_ref(), flags, || {
            ElfLibrary::from_file(path.as_ref(), flags)
        })
    }

    /// Load a shared library from bytes. It is the same as dlopen. However, it can also be used in the no_std environment,
    /// and it will look for dependent libraries in those manually opened dynamic libraries.
    #[inline]
    pub fn dlopen_from_binary(
        bytes: &[u8],
        path: impl AsRef<str>,
        flags: OpenFlags,
    ) -> Result<ElfLibrary> {
        dlopen_impl(path.as_ref(), flags, || {
            ElfLibrary::from_binary(bytes, path.as_ref(), flags)
        })
    }
}

/// The context for a `dlopen` operation.
///
/// Manages the acquisition of the global lock, tracking of newly loaded libraries,
/// and handling resource cleanup if the operation fails.
struct OpenContext<'a> {
    /// The write lock guard for the global library manager.
    /// Can be temporarily dropped to avoid deadlocks during relocation.
    lock: Option<RwLockWriteGuard<'a, Manager>>,
    /// Newly loaded libraries that haven't been finalized yet.
    new_libs: Vec<Option<ElfDylib>>,
    /// The flattened set of all dependencies involved in this load operation.
    dep_libs: Vec<RelocatedDylib>,
    /// Tracks the source of each library in `dep_libs`.
    /// If `Some(j)`, the library is the `j`-th element in `new_libs`.
    /// If `None`, the library was already present in the global manager.
    dep_source: Vec<Option<usize>>,
    /// Loading flags for this operation.
    flags: OpenFlags,
    /// Initial lengths of the registry maps, used for rollback on failure.
    old_all_len: usize,
    old_global_len: usize,
    /// Indicates if the operation was successfully committed.
    committed: bool,
}

impl<'a> Drop for OpenContext<'a> {
    fn drop(&mut self) {
        // If not committed, roll back changes to the global registry.
        if !self.committed && !self.flags.contains(OpenFlags::CUSTOM_NOT_REGISTER) {
            log::debug!("Destroying newly added dynamic libraries from the global");
            if let Some(mut lock) = self.lock.take() {
                lock.all.truncate(self.old_all_len);
                lock.global.truncate(self.old_global_len);
            } else {
                let mut lock = crate::lock_write!(MANAGER);
                lock.all.truncate(self.old_all_len);
                lock.global.truncate(self.old_global_len);
            }
        }
    }
}

impl<'a> OpenContext<'a> {
    fn new(flags: OpenFlags) -> Self {
        let lock = crate::lock_write!(MANAGER);
        let all_len = lock.all.len();
        let global_len = lock.global.len();
        Self {
            lock: Some(lock),
            new_libs: Vec::new(),
            dep_libs: Vec::new(),
            dep_source: Vec::new(),
            flags,
            old_all_len: all_len,
            old_global_len: global_len,
            committed: false,
        }
    }

    fn check_existing(&mut self, shortname: &str) -> Option<ElfLibrary> {
        loop {
            let state = {
                let lock = self.lock.as_ref().unwrap();
                match lock.all.get(shortname) {
                    Some(lib) => lib.state,
                    None => return None,
                }
            };

            if state.is_relocated() {
                let lock = self.lock.as_ref().unwrap();
                return Some(lock.all.get(shortname).unwrap().get_dylib());
            } else {
                // It's being loaded or relocated
                drop(self.lock.take());
                core::hint::spin_loop();
                self.lock = Some(crate::lock_write!(MANAGER));
                continue;
            }
        }
    }

    fn add_new(&mut self, lib: ElfDylib) -> RelocatedDylib {
        let core = lib.core();
        let relocated = unsafe { RelocatedDylib::from_core(core.clone()) };
        self.new_libs.push(Some(lib));
        relocated
    }

    fn load_deps(&mut self) -> Result<()> {
        let mut cur_pos = 0;
        while cur_pos < self.dep_libs.len() {
            let lib_names = self.dep_libs[cur_pos].needed_libs().to_vec();
            let mut cur_rpath: Option<Box<[ElfPath]>> = None;

            // Should we look up RPATH? Only if the current parent is a NEW library.
            let parent_new_idx = self.dep_source[cur_pos];

            for lib_name in &lib_names {
                // 1. Check if already loaded/managed
                if let Some(lib) = self.lock.as_mut().unwrap().all.get_mut(lib_name) {
                    if !self.dep_libs.iter().any(|d| d.shortname() == lib_name) {
                        self.dep_libs.push(lib.relocated_dylib());
                        self.dep_source.push(None); // Existing lib
                        log::debug!("Use an existing dylib: [{}]", lib.shortname());

                        // Update flags if needed
                        if self
                            .flags
                            .difference(lib.flags())
                            .contains(OpenFlags::RTLD_GLOBAL)
                        {
                            let shortname = lib.shortname().to_owned();
                            log::debug!("Updating library flags for [{}]", shortname);
                            lib.set_flags(self.flags);
                            let core = lib.relocated_dylib();
                            self.lock.as_mut().unwrap().global.insert(shortname, core);
                        }
                    }
                    continue;
                }

                // 2. Resolve RPATH if needed
                let rpath: &[ElfPath] = if let Some(rpath) = &cur_rpath {
                    &**rpath
                } else if let Some(idx) = parent_new_idx {
                    let parent_lib = self.new_libs[idx].as_ref().unwrap();
                    let new_rpath = parent_lib
                        .rpath()
                        .map(|rpath| fixup_rpath(parent_lib.name(), rpath))
                        .unwrap_or(Box::new([]));
                    cur_rpath = Some(new_rpath);
                    &**unsafe { cur_rpath.as_ref().unwrap_unchecked() }
                } else {
                    &[]
                };

                // 3. Find and Load
                find_library(rpath, lib_name, |path| {
                    #[cfg(feature = "fs")]
                    {
                        let new_lib = ElfLibrary::from_file(path.as_str(), self.flags)?;
                        let inner = new_lib.core();
                        let relocated = unsafe { RelocatedDylib::from_core(inner.clone()) };

                        // Register BEFORE pushing to `new_libs` to get correct index?
                        // Original code: set_new_idx(new_libs.len()) THEN push.
                        register(
                            relocated.clone(),
                            self.flags,
                            self.lock.as_mut().unwrap(),
                            *DylibState::default().set_new_idx(self.new_libs.len() as _),
                        );

                        self.dep_libs.push(relocated);
                        self.dep_source.push(Some(self.new_libs.len()));
                        self.new_libs.push(Some(new_lib));
                        Ok(())
                    }
                    #[cfg(not(feature = "fs"))]
                    {
                        let _ = path;
                        Err(find_lib_error(format!("can not find file: {}", lib_name)))
                    }
                })?;
            }
            cur_pos += 1;
        }
        Ok(())
    }

    fn compute_order(&mut self) -> Vec<usize> {
        if self.new_libs.is_empty() {
            return Vec::new();
        }

        #[derive(Clone, Copy)]
        struct Item {
            idx: usize,
            next: usize,
        }
        // Start from the root library if it's new (index 0).
        // If the root is existing, we shouldn't be here unless we support partial new deps,
        // which isn't fully supported by this topological sort rooting strategy.
        // However, for standard dlopen usage, new libs only appear if root is new.
        let mut stack = vec![Item { idx: 0, next: 0 }];
        let mut order = Vec::new();

        'start: while let Some(mut item) = stack.pop() {
            let names = self.new_libs[item.idx].as_ref().unwrap().needed_libs();
            for name in names.iter().skip(item.next) {
                let lib = self.lock.as_mut().unwrap().all.get_mut(*name).unwrap();

                if let Some(idx) = lib.state.get_new_idx() {
                    lib.state.set_relocated();
                    item.next += 1;
                    stack.push(item);
                    stack.push(Item {
                        idx: idx as usize,
                        next: 0,
                    });
                    continue 'start;
                }
            }
            order.push(item.idx);
        }
        order
    }

    fn update_dependency_scopes(&mut self) {
        log::debug!("Updating dependency scopes for new libraries");

        // Retrieve names of newly loaded libraries.
        let new_lib_names: Vec<String> = self
            .dep_source
            .iter()
            .enumerate()
            .filter_map(|(i, src)| src.map(|_| self.dep_libs[i].shortname().to_owned()))
            .collect();

        let lock = self.lock.as_mut().expect("Lock must be held");
        crate::core_impl::register::update_dependency_scopes(lock, &new_lib_names);
    }
}

fn dlopen_impl(
    path: &str,
    flags: OpenFlags,
    f: impl Fn() -> Result<ElfDylib>,
) -> Result<ElfLibrary> {
    let mut ctx = OpenContext::new(flags);

    // 1. Initial Check / Load
    let shortname = path.split('/').next_back().unwrap();
    log::info!("dlopen: Try to open [{}] with [{:?}] ", path, flags);

    if !flags.contains(OpenFlags::CUSTOM_NOT_REGISTER) {
        if let Some(lib) = ctx.check_existing(shortname) {
            log::info!("dlopen: Found existing library [{}]", path);
            ctx.committed = true;
            drop(ctx);
            return Ok(lib);
        } else if let Some(existing) = ctx.lock.as_ref().unwrap().all.get(shortname) {
            // It exists but flags/deps didn't match fully for early return?
            let core = existing.relocated_dylib();
            ctx.dep_libs.push(core);
            ctx.dep_source.push(None);
        } else {
            // Load new
            let lib = f()?;
            let relocated = ctx.add_new(lib);
            register(
                relocated.clone(),
                flags,
                ctx.lock.as_mut().unwrap(),
                *DylibState::default().set_new_idx(0),
            );
            ctx.dep_libs.push(relocated);
            ctx.dep_source.push(Some(0)); // 0-th new lib
        }
    } else {
        let lib = f()?;
        let relocated = ctx.add_new(lib);
        register(
            relocated.clone(),
            flags,
            ctx.lock.as_mut().unwrap(),
            *DylibState::default().set_new_idx(0),
        );
        ctx.dep_libs.push(relocated);
        ctx.dep_source.push(Some(0));
    }

    // 2. Resolve Dependencies
    ctx.load_deps()?;

    ctx.update_dependency_scopes();

    // 3. Relocation Order
    let order = ctx.compute_order();

    // 4. Finalize
    let deps: Arc<[RelocatedDylib]> = Arc::from(ctx.dep_libs.clone());
    let core_relocated = ctx.dep_libs[0].clone();

    {
        let lock = ctx.lock.as_mut().unwrap();
        // Set state to RELOCATING for all deps before dropping lock
        for lib in &ctx.dep_libs {
            if let Some(lib) = lock.all.get_mut(lib.shortname()) {
                lib.state.set_relocating();
            }
        }
    }

    // Release write lock to avoid deadlock in dl_iterate_phdr during relocation
    drop(ctx.lock.take());

    let lazy_scope = create_lazy_scope(&deps, flags);
    let global_libs = {
        let lock = crate::lock_read!(MANAGER);
        lock.global.values().cloned().collect::<Vec<_>>()
    };

    for idx in order {
        let lib = core::mem::take(&mut ctx.new_libs[idx]).expect("Library missing");
        log::debug!("Relocating dylib [{}]", lib.name());
        let is_lazy = lib.is_lazy();

        let scope = if flags.contains(OpenFlags::RTLD_DEEPBIND) {
            deps.iter().chain(global_libs.iter())
        } else {
            global_libs.iter().chain(deps.iter())
        };

        lib.relocator()
            .scope(scope.cloned())
            .lazy(is_lazy)
            .lazy_scope(lazy_scope.clone())
            .relocate()?;
    }

    // Set state to RELOCATED
    {
        let mut lock = crate::lock_write!(MANAGER);
        for lib in &ctx.dep_libs {
            if let Some(lib) = lock.all.get_mut(lib.shortname()) {
                lib.state.set_relocated();
            }
        }
    }

    ctx.committed = true;

    Ok(ElfLibrary {
        inner: core_relocated,
        flags,
        deps: Some(deps),
    })
}

static LD_LIBRARY_PATH: Lazy<Box<[ElfPath]>> = Lazy::new(|| {
    //TODO
    Box::new([])
});
static DEFAULT_PATH: spin::Lazy<Box<[ElfPath]>> = Lazy::new(|| unsafe {
    let v = vec![ElfPath::from_str("/usr/lib").unwrap_unchecked()];
    v.into_boxed_slice()
});
static LD_CACHE: Lazy<Box<[ElfPath]>> = Lazy::new(|| {
    #[cfg(feature = "fs")]
    return crate::utils::cache::build_ld_cache().unwrap();
    #[cfg(not(feature = "fs"))]
    Box::new([])
});

#[inline]
fn fixup_rpath(lib_path: &str, rpath: &str) -> Box<[ElfPath]> {
    if !rpath.contains('$') {
        return parse_path_list(rpath);
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
    parse_path_list(&rpath.to_string().replace("$ORIGIN", dir))
}

/// Parses a colon-separated list of paths into a boxed slice of ElfPath.
#[inline]
fn parse_path_list(s: &str) -> Box<[ElfPath]> {
    s.split(':')
        .filter(|str| !str.is_empty())
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

/// # Safety
/// It is the same as `dlopen`.
#[allow(unused_variables)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *const c_void {
    let mut lib = if filename.is_null() {
        crate::lock_read!(MANAGER)
            .all
            .get_index(0)
            .unwrap()
            .1
            .get_dylib()
    } else {
        #[cfg(feature = "fs")]
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
        #[cfg(not(feature = "fs"))]
        return core::ptr::null();
    };
    Box::into_raw(Box::new(core::mem::take(&mut lib.deps).unwrap())) as _
}
