use crate::{
    OpenFlags, Result,
    core_impl::loader::{DylibExt, ElfDylib, ElfLibrary, LoadedDylib, create_lazy_scope},
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
use core::ffi::{CStr, c_char, c_int, c_void};
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

fn get_env(name: &str) -> Option<&'static str> {
    unsafe {
        let mut cur = crate::core_impl::types::ENVP;
        if cur.is_null() {
            return None;
        }
        while !(*cur).is_null() {
            if let Ok(env) = CStr::from_ptr(*cur).to_str() {
                if let Some((k, v)) = env.split_once('=') {
                    if k == name {
                        return Some(v);
                    }
                }
            }
            cur = cur.add(1);
        }
    }
    None
}

impl ElfLibrary {
    /// Get the main executable as an `ElfLibrary`. It is the same as `dlopen(NULL, RTLD_NOW)`.
    pub fn this() -> Result<ElfLibrary> {
        let reader = crate::lock_read!(MANAGER);
        if let Some((_, global)) = reader.all.get_index(0) {
            Ok(global.get_lib())
        } else {
            Err(find_lib_error("main executable not found".to_owned()))
        }
    }

    /// Load a shared library from a specified path. It is the same as dlopen.
    ///
    /// # Example
    /// ```no_run
    /// # use dlopen_rs::{ElfLibrary, OpenFlags};
    ///
    /// let path = "/path/to/library.so";
    /// let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_LOCAL).expect("Failed to load library");
    /// ```
    pub fn dlopen(path: impl AsRef<str>, flags: OpenFlags) -> Result<ElfLibrary> {
        dlopen_impl(path.as_ref(), flags, |p| ElfLibrary::from_file(p))
    }

    /// Load a shared library from bytes. It is the same as dlopen. However, it can also be used in the no_std environment,
    /// and it will look for dependent libraries in those manually opened dynamic libraries.
    pub fn dlopen_from_binary(
        bytes: &[u8],
        path: impl AsRef<str>,
        flags: OpenFlags,
    ) -> Result<ElfLibrary> {
        dlopen_impl(path.as_ref(), flags, |_| {
            ElfLibrary::from_binary(bytes, path.as_ref())
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
    dep_libs: Vec<LoadedDylib>,
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
        if !self.committed {
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
    fn new(mut flags: OpenFlags) -> Self {
        if get_env("LD_BIND_NOW").is_some() {
            flags |= OpenFlags::RTLD_NOW;
        }
        let lock = crate::lock_write!(MANAGER);
        let all_len = lock.all.len();
        let global_len = lock.global.len();
        Self {
            lock: Some(lock),
            new_libs: Vec::new(),
            dep_libs: Vec::new(),
            flags,
            old_all_len: all_len,
            old_global_len: global_len,
            committed: false,
        }
    }

    fn try_existing(&mut self, path: &str) -> Option<ElfLibrary> {
        let shortname = path.rsplit_once('/').map_or(path, |(_, name)| name);
        let mut lib = loop {
            let state = {
                let lock = self.lock.as_ref().unwrap();
                match lock.all.get(shortname) {
                    Some(lib) => lib.state,
                    None => return None,
                }
            };

            if state.is_relocated() {
                let lock = self.lock.as_ref().unwrap();
                break lock.all.get(shortname).unwrap().get_lib();
            } else {
                // It's being loaded or relocated
                drop(self.lock.take());
                core::hint::spin_loop();
                self.lock = Some(crate::lock_write!(MANAGER));
                continue;
            }
        };

        log::info!("dlopen: Found existing library [{}]", path);
        let flags = self.flags;
        if (flags.contains(OpenFlags::RTLD_GLOBAL) && !lib.flags.contains(OpenFlags::RTLD_GLOBAL))
            || (flags.contains(OpenFlags::RTLD_NODELETE)
                && !lib.flags.contains(OpenFlags::RTLD_NODELETE))
        {
            log::debug!("Updating library flags for [{}]", shortname);
            let lock = self.lock.as_mut().unwrap();
            if let Some(entry) = lock.all.get_mut(shortname) {
                entry.set_flags(
                    entry.flags() | (flags & (OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NODELETE)),
                );
            }
            if flags.contains(OpenFlags::RTLD_GLOBAL) {
                if let Some(entry) = lock.all.get(shortname) {
                    let core = entry.dylib();
                    lock.global.insert(shortname.to_owned(), core);
                }
            }
            // Update the flags in the current handle to reflect promotion
            lib.flags |= flags & (OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NODELETE);
        }
        self.committed = true;
        Some(lib)
    }

    fn register_new(&mut self, lib: ElfDylib) -> LoadedDylib {
        let core = lib.core();
        let relocated = unsafe { LoadedDylib::from_core(core.clone()) };
        let new_idx = self.new_libs.len();

        register(
            relocated.clone(),
            self.flags,
            self.lock.as_mut().unwrap(),
            *DylibState::default().set_new_idx(new_idx as _),
        );

        self.dep_libs.push(relocated.clone());
        self.new_libs.push(Some(lib));

        relocated
    }

    fn load_deps(&mut self) -> Result<()> {
        let mut cur_pos = 0;
        while cur_pos < self.dep_libs.len() {
            let lib_names = self.dep_libs[cur_pos].needed_libs().to_vec();
            let mut cur_paths: Option<(Box<[ElfPath]>, Box<[ElfPath]>)> = None;

            // Should we look up RPATH/RUNPATH? Only if the current parent is a NEW library.
            let parent_new_idx = self
                .lock
                .as_mut()
                .unwrap()
                .all
                .get(self.dep_libs[cur_pos].shortname())
                .unwrap()
                .state
                .get_new_idx()
                .map(|idx| idx as usize);

            for lib_name in &lib_names {
                // 1. Check if already loaded/managed
                if let Some(lib) = self.lock.as_mut().unwrap().all.get_mut(lib_name) {
                    if !self.dep_libs.iter().any(|d| d.shortname() == lib_name) {
                        self.dep_libs.push(lib.dylib());
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
                            let core = lib.dylib();
                            self.lock.as_mut().unwrap().global.insert(shortname, core);
                        }
                    }
                    continue;
                }

                // 2. Resolve RPATH/RUNPATH if needed
                let (rpath, runpath): (&[ElfPath], &[ElfPath]) = if let Some((r, ru)) = &cur_paths {
                    (&**r, &**ru)
                } else if let Some(idx) = parent_new_idx {
                    let parent_lib = self.new_libs[idx].as_ref().unwrap();
                    let new_rpath = parent_lib
                        .rpath()
                        .map(|r| fixup_rpath(parent_lib.name(), r))
                        .unwrap_or(Box::new([]));
                    let new_runpath = parent_lib
                        .runpath()
                        .map(|r| fixup_rpath(parent_lib.name(), r))
                        .unwrap_or(Box::new([]));
                    cur_paths = Some((new_rpath, new_runpath));
                    let (r, ru) = unsafe { cur_paths.as_ref().unwrap_unchecked() };
                    (&**r, &**ru)
                } else {
                    (&[], &[])
                };

                // 3. Find and Load
                find_library(rpath, runpath, lib_name, |path| {
                    let new_lib = ElfLibrary::from_file(path.as_str())?;
                    self.register_new(new_lib);
                    Ok(())
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

        let lock = self.lock.as_mut().expect("Lock must be held");
        // Retrieve names of newly loaded libraries.
        let new_lib_names: Vec<String> = self
            .dep_libs
            .iter()
            .filter(|lib| {
                lock.all
                    .get(lib.shortname())
                    .map_or(false, |entry| entry.state.get_new_idx().is_some())
            })
            .map(|lib| lib.shortname().to_owned())
            .collect();

        crate::core_impl::register::update_dependency_scopes(lock, &new_lib_names);
    }

    /// Sets the state of all involved libraries to `RELOCATING`.
    fn set_relocating(&mut self) {
        let lock = self.lock.as_mut().expect("Lock must be held");
        for lib in &self.dep_libs {
            if let Some(entry) = lock.all.get_mut(lib.shortname()) {
                entry.state.set_relocating();
            }
        }
    }

    /// Sets the state of all involved libraries to `RELOCATED`.
    /// Note: This acquires a new write lock as the context's lock might have been dropped.
    fn set_relocated(&self) {
        let mut lock = crate::lock_write!(MANAGER);
        for lib in &self.dep_libs {
            if let Some(entry) = lock.all.get_mut(lib.shortname()) {
                entry.state.set_relocated();
            }
        }
    }

    /// Performs the relocation for all new libraries in the specified order.
    fn relocate(&mut self, order: &[usize], deps: &Arc<[LoadedDylib]>) -> Result<()> {
        // Set state to RELOCATING for all deps before dropping lock
        self.set_relocating();

        // Release write lock to avoid deadlock in dl_iterate_phdr during relocation
        drop(self.lock.take());

        let lazy_scope = create_lazy_scope(deps, self.flags);
        let global_libs = {
            let lock = crate::lock_read!(MANAGER);
            lock.global.values().cloned().collect::<Vec<_>>()
        };

        for &idx in order {
            let lib = core::mem::take(&mut self.new_libs[idx]).expect("Library missing");
            log::debug!("Relocating dylib [{}]", lib.name());
            let is_lazy = if self.flags.contains(OpenFlags::RTLD_NOW) {
                false
            } else if self.flags.contains(OpenFlags::RTLD_LAZY) {
                true
            } else {
                lib.is_lazy()
            };

            let scope = if self.flags.contains(OpenFlags::RTLD_DEEPBIND) {
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
        self.set_relocated();

        Ok(())
    }

    /// Returns an `Arc` slice of all dependencies.
    fn get_deps(&self) -> Arc<[LoadedDylib]> {
        Arc::from(self.dep_libs.as_slice())
    }

    /// Finalizes the operation and returns the `ElfLibrary`.
    fn finish(mut self, deps: Arc<[LoadedDylib]>) -> ElfLibrary {
        self.committed = true;
        let core = deps[0].clone();
        ElfLibrary {
            inner: core,
            flags: self.flags,
            deps: Some(deps),
        }
    }
}

fn dlopen_impl(
    path: &str,
    flags: OpenFlags,
    f: impl Fn(&str) -> Result<ElfDylib>,
) -> Result<ElfLibrary> {
    let mut ctx = OpenContext::new(flags);

    // 1. Initial Check / Load
    log::info!("dlopen: Try to open [{}] with [{:?}] ", path, ctx.flags);

    if let Some(lib) = ctx.try_existing(path) {
        return Ok(lib);
    }

    if flags.contains(OpenFlags::RTLD_NOLOAD) {
        return Err(find_lib_error(format!("can not find file: {}", path)));
    }

    // Load new library
    let lib = if !path.contains('/') {
        let mut loaded_lib = None;
        find_library(&[], &[], path, |p| {
            let lib = f(p.as_str())?;
            loaded_lib = Some(lib);
            Ok(())
        })?;
        loaded_lib.ok_or_else(|| find_lib_error(format!("can not find file: {}", path)))?
    } else {
        f(path)?
    };
    ctx.register_new(lib);

    // 2. Resolve Dependencies
    ctx.load_deps()?;

    // 3. Update Dependency Scopes
    ctx.update_dependency_scopes();

    // 4. Relocation Order
    let order = ctx.compute_order();

    // 5. Relocation
    let deps = ctx.get_deps();
    ctx.relocate(&order, &deps)?;

    // 6. Finalize
    Ok(ctx.finish(deps))
}

static LD_LIBRARY_PATH: Lazy<Box<[ElfPath]>> = Lazy::new(|| {
    if let Some(path) = get_env("LD_LIBRARY_PATH") {
        parse_path_list(path)
    } else {
        Box::new([])
    }
});
static DEFAULT_PATH: spin::Lazy<Box<[ElfPath]>> = Lazy::new(|| unsafe {
    let v = vec![
        ElfPath::from_str("/lib").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib").unwrap_unchecked(),
        ElfPath::from_str("/lib64").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib64").unwrap_unchecked(),
        ElfPath::from_str("/lib/x86_64-linux-gnu").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib/x86_64-linux-gnu").unwrap_unchecked(),
        ElfPath::from_str("/lib/aarch64-linux-gnu").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib/aarch64-linux-gnu").unwrap_unchecked(),
        ElfPath::from_str("/lib/riscv64-linux-gnu").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib/riscv64-linux-gnu").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib/aarch64-linux-gnu").unwrap_unchecked(),
        ElfPath::from_str("/usr/aarch64-linux-gnu/lib").unwrap_unchecked(),
    ];
    v.into_boxed_slice()
});
static LD_CACHE: Lazy<Option<crate::utils::cache::LdCache>> =
    Lazy::new(|| crate::utils::cache::LdCache::new().ok());

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

fn find_library(
    rpath: &[ElfPath],
    runpath: &[ElfPath],
    lib_name: &str,
    mut f: impl FnMut(&ElfPath) -> Result<()>,
) -> Result<()> {
    // Search order: DT_RPATH -> LD_LIBRARY_PATH -> DT_RUNPATH -> LD_CACHE -> DEFAULT_PATH
    // If DT_RUNPATH is present, DT_RPATH is ignored.

    // 1. DT_RPATH
    if runpath.is_empty() {
        for path in rpath {
            let file_path = path.join(lib_name);
            if f(&file_path).is_ok() {
                return Ok(());
            }
        }
    }

    // 2. LD_LIBRARY_PATH
    for path in LD_LIBRARY_PATH.iter() {
        let file_path = path.join(lib_name);
        if f(&file_path).is_ok() {
            return Ok(());
        }
    }

    // 3. DT_RUNPATH
    for path in runpath {
        let file_path = path.join(lib_name);
        if f(&file_path).is_ok() {
            return Ok(());
        }
    }

    // 4. LD_CACHE
    if let Some(cache) = &*LD_CACHE {
        if let Some(path) = cache.lookup(lib_name) {
            log::debug!("Found [{}] in LD_CACHE: [{}]", lib_name, path);
            if let Ok(path) = ElfPath::from_str(&path) {
                if f(&path).is_ok() {
                    return Ok(());
                }
            }
        }
    }

    // 5. DEFAULT_PATH
    for path in DEFAULT_PATH.iter() {
        let file_path = path.join(lib_name);
        if f(&file_path).is_ok() {
            return Ok(());
        }
    }

    Err(find_lib_error(format!("can not find file: {}", lib_name)))
}

/// # Safety
/// It is the same as `dlopen`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *const c_void {
    let mut lib = if filename.is_null() {
        if let Ok(this) = ElfLibrary::this() {
            this
        } else {
            return core::ptr::null();
        }
    } else {
        let flags = OpenFlags::from_bits_retain(flags as _);
        let filename = unsafe { core::ffi::CStr::from_ptr(filename) };
        let Ok(path) = filename.to_str() else {
            return core::ptr::null();
        };
        if let Ok(lib) = ElfLibrary::dlopen(path, flags) {
            lib
        } else {
            return core::ptr::null();
        }
    };
    let Some(deps) = core::mem::take(&mut lib.deps) else {
        return core::ptr::null();
    };
    Box::into_raw(Box::new(deps)) as _
}
