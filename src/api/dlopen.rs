use crate::{
    OpenFlags, Result,
    core_impl::{
        loader::{DylibExt, ElfDylib, ElfLibrary, LoadResult, LoadedDylib, create_lazy_scope},
        register::{DylibState, GlobalDylib, MANAGER, Manager, register},
        traits::AsFilename,
    },
    error::find_lib_error,
    utils::ld_cache::LdCache,
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
    pub fn this() -> ElfLibrary {
        let reader = crate::lock_read!(MANAGER);
        reader
            .get_index(0)
            .expect("Main executable must be initialized")
            .1
            .get_lib()
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
    pub fn dlopen(path: impl AsFilename, flags: OpenFlags) -> Result<ElfLibrary> {
        dlopen_impl(path.as_filename(), flags, None)
    }

    /// Load a shared library from bytes. It is the same as dlopen. However, it can also be used in the no_std environment,
    /// and it will look for dependent libraries in those manually opened dynamic libraries.
    pub fn dlopen_from_binary(
        bytes: &[u8],
        path: impl AsFilename,
        flags: OpenFlags,
    ) -> Result<ElfLibrary> {
        dlopen_impl(path.as_filename(), flags, Some(bytes))
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
    /// Metadata needed for each newly loaded library in the current operation.
    new_libs: Vec<Option<ElfDylib>>,
    /// Names of libraries that were added to the global registry in this operation.
    added_names: Vec<String>,
    /// The flattened set of all dependencies involved in this load operation.
    dep_libs: Vec<LoadedDylib>,
    /// Loading flags for this operation.
    flags: OpenFlags,
    /// Indicates if the operation was successfully committed.
    committed: bool,
}

impl<'a> Drop for OpenContext<'a> {
    fn drop(&mut self) {
        // If not committed, roll back changes to the global registry.
        if !self.committed {
            log::debug!("Destroying newly added dynamic libraries from the global");
            let mut lock = self
                .lock
                .take()
                .unwrap_or_else(|| crate::lock_write!(MANAGER));
            for name in &self.added_names {
                lock.remove(name);
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
        Self {
            lock: Some(lock),
            new_libs: Vec::new(),
            added_names: Vec::new(),
            dep_libs: Vec::new(),
            flags,
            committed: false,
        }
    }

    fn try_existing(&mut self, path: &str) -> Option<ElfLibrary> {
        let shortname = path.rsplit_once('/').map_or(path, |(_, name)| name);
        if let Some(lib) = self.wait_for_library(shortname) {
            let elf_lib = lib.get_lib();
            log::info!("dlopen: Found existing library [{}]", path);
            self.lock
                .as_mut()
                .expect("Lock must be held")
                .promote(shortname, self.flags);
            self.committed = true;
            return Some(elf_lib);
        }
        None
    }

    fn wait_for_library(&mut self, shortname: &str) -> Option<GlobalDylib> {
        loop {
            let entry = {
                let lock = self.lock.as_ref().expect("Lock must be held");
                lock.get(shortname).cloned()
            };

            match entry {
                Some(lib) if lib.state.is_relocated() => return Some(lib),
                Some(lib) if self.added_names.iter().any(|n| n == shortname) => {
                    // It's a library being loaded by the current thread in this dlopen session.
                    // We must not wait, otherwise we deadlock.
                    return Some(lib);
                }
                Some(_) => {
                    // It's being loaded or relocated by another thread
                    drop(self.lock.take());
                    core::hint::spin_loop();
                    self.lock = Some(crate::lock_write!(MANAGER));
                }
                None => return None,
            }
        }
    }

    fn register_new(&mut self, lib: ElfDylib) -> LoadedDylib {
        let core = lib.core();
        let relocated = unsafe { LoadedDylib::from_core(core.clone()) };
        let new_idx = self.new_libs.len();

        let shortname = relocated.shortname().to_owned();
        register(
            relocated.clone(),
            self.flags,
            self.lock.as_mut().expect("Lock must be held"),
            *DylibState::default().set_new_idx(new_idx as _),
        );

        self.dep_libs.push(relocated.clone());
        self.added_names.push(shortname);
        self.new_libs.push(Some(lib));

        relocated
    }

    fn try_use_existing(&mut self, shortname: &str) -> bool {
        // If it's already in our current dependency chain, we don't need to wait or promote.
        // It might be one of our 'added_names' (newly loaded) or an existing one we already found.
        if self.dep_libs.iter().any(|d| d.shortname() == shortname) {
            return true;
        }

        if let Some(lib) = self.wait_for_library(shortname) {
            // Found an existing library from a PREVIOUS dlopen (already relocated)
            // or one being loaded by another thread (we waited for it).
            self.dep_libs.push(lib.dylib());
            log::debug!("Use an existing dylib: [{}]", lib.shortname());
            self.lock
                .as_mut()
                .expect("Lock must be held")
                .promote(shortname, self.flags);
            return true;
        }
        false
    }

    fn load_and_register(
        &mut self,
        p: &ElfPath,
        bytes: Option<&[u8]>,
    ) -> Result<Option<Vec<String>>> {
        match ElfLibrary::load(p.as_str(), bytes)? {
            LoadResult::Dylib(lib) => {
                self.register_new(lib);
                Ok(None)
            }
            LoadResult::Script(libs) => Ok(Some(libs)),
        }
    }

    fn load_deps(&mut self) -> Result<()> {
        let mut cur_pos = 0;
        while cur_pos < self.dep_libs.len() {
            let lib_names = self.dep_libs[cur_pos].needed_libs().to_vec();
            let mut cur_paths: Option<(Box<[ElfPath]>, Box<[ElfPath]>)> = None;

            // Should we look up RPATH/RUNPATH? Only if the current parent is a NEW library.
            let parent_new_idx = {
                let lock = self.lock.as_mut().expect("Lock must be held");
                lock.get(self.dep_libs[cur_pos].shortname())
                    .expect("Library must be registered")
                    .state
                    .get_new_idx()
                    .map(|idx| idx as usize)
            };

            for lib_name in lib_names {
                let (rpath, runpath): (&[ElfPath], &[ElfPath]) = if let Some((r, ru)) = &cur_paths {
                    (&**r, &**ru)
                } else if let Some(idx) = parent_new_idx {
                    let parent_lib: &ElfDylib = self.new_libs[idx]
                        .as_ref()
                        .expect("New library must be available");
                    let new_rpath = parent_lib
                        .rpath()
                        .map(|r| fixup_rpath(parent_lib.name(), r))
                        .unwrap_or_default();
                    let new_runpath = parent_lib
                        .runpath()
                        .map(|r| fixup_rpath(parent_lib.name(), r))
                        .unwrap_or_default();
                    cur_paths = Some((new_rpath, new_runpath));
                    let (r, ru) = unsafe { cur_paths.as_ref().unwrap_unchecked() };
                    (&**r, &**ru)
                } else {
                    (&[], &[])
                };

                self.find_library(rpath, runpath, &lib_name, None)?;
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
            let names = self.new_libs[item.idx]
                .as_ref()
                .expect("New library must be available")
                .needed_libs();
            for name in names.iter().skip(item.next) {
                let lib = self
                    .lock
                    .as_mut()
                    .expect("Lock must be held")
                    .get_mut(*name)
                    .expect("Library must be registered");

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
        let new_lib_names = self
            .new_libs
            .iter()
            .filter_map(|lib_opt| lib_opt.as_ref())
            .map(|lib| lib.short_name());

        crate::core_impl::register::update_dependency_scopes(lock, new_lib_names);
    }

    /// Sets the state of all involved libraries to `RELOCATING`.
    fn set_relocating(&mut self) {
        let lock = self.lock.as_mut().expect("Lock must be held");
        for lib in &self.dep_libs {
            lock.get_mut(lib.shortname())
                .expect("Library must be registered")
                .state
                .set_relocating();
        }

        // Release write lock to avoid deadlock in dl_iterate_phdr during relocation
        drop(self.lock.take());
    }

    /// Sets the state of all involved libraries to `RELOCATED`.
    /// Note: This acquires a new write lock as the context's lock might have been dropped.
    fn set_relocated(&self) {
        let mut lock = crate::lock_write!(MANAGER);
        for lib in &self.dep_libs {
            lock.get_mut(lib.shortname())
                .expect("Library must be registered")
                .state
                .set_relocated();
        }
    }

    /// Performs the relocation for all new libraries in the specified order.
    fn relocate(&mut self, order: &[usize], deps: &Arc<[LoadedDylib]>) -> Result<()> {
        // Set state to RELOCATING for all deps before dropping lock
        self.set_relocating();

        let lazy_scope = create_lazy_scope(deps, self.flags);
        let global_libs = {
            let lock = crate::lock_read!(MANAGER);
            lock.global_values().cloned().collect::<Vec<_>>()
        };

        for &idx in order {
            let lib = core::mem::take(&mut self.new_libs[idx]).expect("Library missing");
            log::debug!("Relocating dylib [{}]", lib.name());
            let is_lazy = if self.flags.is_now() {
                false
            } else if self.flags.is_lazy() {
                true
            } else {
                lib.is_lazy()
            };

            let scope = if self.flags.is_deepbind() {
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
        let lock = self.lock.as_ref().expect("Lock must be held");
        let shortname = self.dep_libs[0].shortname();
        lock.get(shortname)
            .expect("Root library must be registered")
            .deps
            .clone()
            .expect("Dependency scope must be computed")
    }

    /// Finalizes the operation and returns the `ElfLibrary`.
    fn finish(mut self, deps: Arc<[LoadedDylib]>) -> ElfLibrary {
        self.committed = true;
        let core = deps[0].clone();
        ElfLibrary {
            inner: core,
            deps: Some(deps),
        }
    }

    fn load_root(&mut self, path: &str, bytes: Option<&[u8]>) -> Result<Option<ElfLibrary>> {
        if let Some(lib) = self.try_existing(path) {
            return Ok(Some(lib));
        }

        if self.flags.is_noload() {
            return Err(find_lib_error(format!("can not find file: {}", path)));
        }

        self.find_library(&[], &[], path, bytes)?;
        Ok(None)
    }

    fn find_library(
        &mut self,
        rpath: &[ElfPath],
        runpath: &[ElfPath],
        lib_name: &str,
        bytes: Option<&[u8]>,
    ) -> Result<()> {
        let shortname = lib_name.rsplit_once('/').map_or(lib_name, |(_, name)| name);
        if self.try_use_existing(shortname) {
            return Ok(());
        }

        // 1. Absolute or relative path (contains '/')
        if lib_name.contains('/') {
            if let Ok(path) = ElfPath::from_str(lib_name) {
                return self.try_load_internal(rpath, runpath, &path, bytes);
            }
        }

        // Search order: DT_RPATH -> LD_LIBRARY_PATH -> DT_RUNPATH -> LD_CACHE -> DEFAULT_PATH
        let rpath_dirs = if runpath.is_empty() { rpath } else { &[] };
        for dir in rpath_dirs
            .iter()
            .chain(LD_LIBRARY_PATH.iter())
            .chain(runpath.iter())
        {
            if self
                .try_load_internal(rpath, runpath, &dir.join(lib_name), bytes)
                .is_ok()
            {
                return Ok(());
            }
        }

        // 4. LD_CACHE
        if let Some(cache) = &*LD_CACHE {
            if let Some(path) = cache.lookup(lib_name) {
                if let Ok(path) = ElfPath::from_str(&path) {
                    if self.try_load_internal(rpath, runpath, &path, bytes).is_ok() {
                        return Ok(());
                    }
                }
            }
        }

        // 5. DEFAULT_PATH
        for dir in DEFAULT_PATH.iter() {
            if self
                .try_load_internal(rpath, runpath, &dir.join(lib_name), bytes)
                .is_ok()
            {
                return Ok(());
            }
        }

        Err(find_lib_error(format!(
            "can not find library: {}",
            lib_name
        )))
    }

    fn try_load_internal(
        &mut self,
        rpath: &[ElfPath],
        runpath: &[ElfPath],
        path: &ElfPath,
        bytes: Option<&[u8]>,
    ) -> Result<()> {
        let Some(libs) = self.load_and_register(path, bytes)? else {
            return Ok(());
        };
        for lib in libs {
            self.find_library(rpath, runpath, &lib, None)?;
        }
        Ok(())
    }
}

fn dlopen_impl(path: &str, flags: OpenFlags, bytes: Option<&[u8]>) -> Result<ElfLibrary> {
    let mut ctx = OpenContext::new(flags);

    // 1. Initial Check / Load
    log::info!("dlopen: Try to open [{}] with [{:?}] ", path, ctx.flags);

    if let Some(lib) = ctx.load_root(path, bytes)? {
        return Ok(lib);
    }

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
static DEFAULT_PATH: Lazy<Box<[ElfPath]>> = Lazy::new(|| unsafe {
    let v = vec![
        ElfPath::from_str("/lib").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib").unwrap_unchecked(),
        ElfPath::from_str("/lib64").unwrap_unchecked(),
        ElfPath::from_str("/usr/lib64").unwrap_unchecked(),
    ];
    v.into_boxed_slice()
});
static LD_CACHE: Lazy<Option<LdCache>> = Lazy::new(|| LdCache::new().ok());

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

/// # Safety
/// It is the same as `dlopen`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *const c_void {
    let lib = if filename.is_null() {
        ElfLibrary::this()
    } else {
        let flags = OpenFlags::from_bits_retain(flags as _);
        let filename = unsafe { CStr::from_ptr(filename) };
        let Ok(path) = filename.to_str() else {
            return core::ptr::null();
        };
        if let Ok(lib) = ElfLibrary::dlopen(path, flags) {
            lib
        } else {
            return core::ptr::null();
        }
    };
    Box::into_raw(Box::new(lib)) as _
}
