use crate::{
    ElfLibrary, OpenFlags,
    core_impl::loader::{DylibExt, LoadedDylib},
    core_impl::types::FileIdentity,
};
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    collections::{btree_set::BTreeSet, vec_deque::VecDeque},
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::ffi::{c_int, c_void};
use hashbrown::HashMap;
use indexmap::IndexMap;
use spin::{Lazy, RwLock};

#[macro_export]
macro_rules! lock_write {
    ($lock:expr) => {{ $lock.write() }};
}

#[macro_export]
macro_rules! lock_read {
    ($lock:expr) => {{ $lock.read() }};
}

impl Drop for ElfLibrary {
    fn drop(&mut self) {
        let mut removed_libs = Vec::new();
        {
            let mut lock = lock_write!(MANAGER);
            let shortname = self.inner.shortname();
            let Some(entry) = lock.get(shortname) else {
                return;
            };

            if entry.flags.is_nodelete() {
                return;
            }

            let ref_count = unsafe { self.inner.core_ref().strong_count() };
            let has_global = entry.flags.is_global();
            // Dylib ref in 'all' map + dylib ref in 'deps' list of itself + global ref (if present) + handle's 'inner' ref
            debug_assert!(self.deps.is_some());
            let threshold = 3 + has_global as usize;

            log::debug!(
                "Drop ElfLibrary [{}], ref count: {}, threshold: {}",
                self.inner.name(),
                ref_count,
                threshold
            );

            if ref_count == threshold {
                log::info!("Destroying dylib [{}]", self.inner.name());
                removed_libs.push(self.inner.clone());

                lock.remove(shortname);

                // Check dependencies
                if let Some(deps) = self.deps.as_ref() {
                    for dep in deps.iter().skip(1) {
                        let dep_shortname = dep.shortname();
                        let Some(dep_lib) = lock.get(dep_shortname) else {
                            continue;
                        };
                        if dep_lib.flags.is_nodelete() {
                            continue;
                        }
                        debug_assert!(
                            dep_lib.deps.is_some(),
                            "Dependency [{}] must have its deps set",
                            dep.name()
                        );
                        // Dylib ref in 'all' map + dylib ref in 'deps' list of itself + global ref (if present)
                        let dep_threshold = 3 + dep_lib.flags.is_global() as usize;

                        if unsafe { dep.core_ref().strong_count() } == dep_threshold {
                            log::info!("Destroying dylib [{}]", dep.name());
                            removed_libs.push(dep.clone());
                            lock.remove(dep_shortname);
                        }
                    }
                }
            }
        }
        for lib in removed_libs {
            let base = lib.base();
            let range = base..(base + lib.mapped_len());
            finalize(base as *mut _, Some(range));
        }
    }
}

/// Represents the current lifecycle state of a dynamic library.
#[derive(Clone, Copy, Default)]
pub(crate) enum DylibState {
    #[default]
    New,
    Relocating,
    Relocated,
}

impl DylibState {
    /// Returns true if the library is fully relocated.
    #[inline]
    pub(crate) fn is_relocated(&self) -> bool {
        matches!(self, Self::Relocated)
    }

    /// Transitions the state to Relocated.
    #[inline]
    pub(crate) fn set_relocated(&mut self) -> &mut Self {
        *self = Self::Relocated;
        self
    }

    /// Transitions the state to Relocating.
    #[inline]
    pub(crate) fn set_relocating(&mut self) -> &mut Self {
        *self = Self::Relocating;
        self
    }
}

/// A handle to a dynamic library within the global registry.
///
/// This struct wraps a `RelocatedDylib` with additional metadata such as
/// loading flags, computed dependency scope, and lifecycle state.
#[derive(Clone)]
pub(crate) struct GlobalDylib {
    inner: LoadedDylib,
    pub(crate) flags: OpenFlags,
    /// The full dependency scope (Searchlist) used for symbol resolution,
    /// calculated via BFS starting from this library.
    pub(crate) deps: Option<Arc<[LoadedDylib]>>,
    /// Current state in the load/relocate lifecycle.
    pub(crate) state: DylibState,
    /// Alternative names for this library (symlinks, alias paths, etc.).
    /// Mirrors glibc's l_libname linked list.
    pub(crate) libnames: Vec<String>,
}

unsafe impl Send for GlobalDylib {}
unsafe impl Sync for GlobalDylib {}

impl GlobalDylib {
    #[inline]
    pub(crate) fn dylib(&self) -> LoadedDylib {
        self.inner.clone()
    }

    #[inline]
    pub(crate) fn dylib_ref(&self) -> &LoadedDylib {
        &self.inner
    }

    #[inline]
    pub(crate) fn shortname(&self) -> &str {
        self.inner.shortname()
    }
}

/// The global manager for all loaded dynamic libraries.
pub(crate) struct Manager {
    /// Maps a library's short name (e.g., "libc.so.6") to its full metadata.
    /// Uses `IndexMap` to preserve loading order for symbol resolution.
    all: IndexMap<String, GlobalDylib>,
    /// Libraries available in the global symbol scope (RTLD_GLOBAL).
    global: IndexMap<String, LoadedDylib>,
    /// Alias names that resolve to a canonical short name.
    aliases: HashMap<String, String>,
    /// Maps file identities to the canonical short name for fast inode-based lookup.
    identities: HashMap<FileIdentity, String>,
    /// The number of times a new object has been added to the link map.
    adds: u64,
    /// The number of times an object has been removed from the link map.
    subs: u64,
}

impl Manager {
    fn canonical_name_owned(&self, name: &str) -> Option<String> {
        Some(self.get(name)?.shortname().to_owned())
    }

    fn promoted_name(&mut self, name: &str, flags: OpenFlags) -> Option<String> {
        let canonical = self.canonical_name_owned(name)?;
        self.promote(&canonical, flags);
        Some(canonical)
    }

    pub(crate) fn add_global(&mut self, name: String, lib: LoadedDylib) {
        debug_assert!(
            !self.global.contains_key(&name),
            "Library [{}] is already in global scope",
            name
        );
        log::trace!("Adding [{}] to global scope", name);
        self.global.insert(name, lib);
    }

    pub(crate) fn add(&mut self, name: String, mut lib: GlobalDylib) {
        lib.libnames = Vec::new();
        let res = self.all.insert(name.clone(), lib);
        debug_assert!(res.is_none(), "Library [{}] is already registered", name);
        self.adds += 1;
        log::trace!("Registered [{}] in all map", name);
    }

    pub(crate) fn add_alias(&mut self, canonical: &str, alias: &str) {
        debug_assert!(
            self.all.contains_key(canonical),
            "Canonical library [{}] must be registered before adding aliases",
            canonical
        );

        if self.all.contains_key(alias) {
            log::trace!(
                "Skipping alias [{}] for [{}]: the name is already used as a canonical key",
                alias,
                canonical
            );
            return;
        }

        if let Some(existing) = self.aliases.get(alias) {
            if existing != canonical {
                log::trace!(
                    "Skipping alias [{}] for [{}]: it already resolves to [{}]",
                    alias,
                    canonical,
                    existing
                );
            }
            return;
        }

        let lib = self
            .all
            .get_mut(canonical)
            .expect("Canonical library must be registered before adding aliases");

        log::trace!("Adding alias [{}] to library [{}]", alias, canonical);
        lib.libnames.push(alias.to_owned());
        self.aliases.insert(alias.to_owned(), canonical.to_owned());
    }

    pub(crate) fn add_identity(&mut self, identity: FileIdentity, name: &str) {
        // Newest wins; identical inode implies same physical file.
        self.identities.insert(identity, name.to_owned());
    }

    pub(crate) fn remove(&mut self, shortname: &str) {
        let lib = self
            .all
            .shift_remove(shortname)
            .expect("Library is not registered");
        self.subs += 1;
        let res = self.global.shift_remove(shortname);
        debug_assert!(
            lib.flags.is_global() == res.is_some(),
            "Inconsistent global scope state when removing [{}]",
            shortname
        );
        for alias in &lib.libnames {
            self.aliases.remove(alias);
        }
        // Remove any identity aliases pointing to this shortname.
        self.identities.retain(|_, v| v != shortname);
    }

    #[inline]
    pub(crate) fn get(&self, name: &str) -> Option<&GlobalDylib> {
        // Primary lookup by canonical shortname.
        if let Some(lib) = self.all.get(name) {
            return Some(lib);
        }
        self.aliases
            .get(name)
            .and_then(|canonical| self.all.get(canonical))
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut GlobalDylib> {
        // Primary lookup.
        if self.all.contains_key(name) {
            return self.all.get_mut(name);
        }
        let canonical = self.aliases.get(name)?.clone();
        self.all.get_mut(&canonical)
    }

    #[inline]
    pub(crate) fn all_values(&self) -> indexmap::map::Values<'_, String, GlobalDylib> {
        self.all.values()
    }

    #[inline]
    pub(crate) fn global_values(&self) -> indexmap::map::Values<'_, String, LoadedDylib> {
        self.global.values()
    }

    #[inline]
    pub(crate) fn all_iter(&self) -> indexmap::map::Iter<'_, String, GlobalDylib> {
        self.all.iter()
    }

    pub(crate) fn adds(&self) -> u64 {
        self.adds
    }

    pub(crate) fn subs(&self) -> u64 {
        self.subs
    }

    #[inline]
    pub(crate) fn get_by_identity(&self, identity: &FileIdentity) -> Option<&GlobalDylib> {
        self.identities
            .get(identity)
            .and_then(|name| self.all.get(name))
    }

    #[inline]
    pub(crate) fn main_library(&self) -> Option<ElfLibrary> {
        let (_, lib) = self.all.get_index(0)?;
        Some(ElfLibrary {
            inner: lib.inner.clone(),
            deps: lib.deps.clone(),
        })
    }

    pub(crate) fn cache_deps(&mut self, name: &str, deps: Arc<[LoadedDylib]>) {
        self.get_mut(name).expect("Library must be registered").deps = Some(deps);
    }

    pub(crate) fn canonical_direct_deps(&self, lib: &LoadedDylib) -> Box<[String]> {
        let mut deps = Vec::with_capacity(lib.needed_libs().len());
        let mut seen = BTreeSet::new();

        for needed in lib.needed_libs() {
            let Some(dep) = self.get(needed) else {
                continue;
            };
            let shortname = dep.shortname().to_owned();
            if seen.insert(shortname.clone()) {
                deps.push(shortname);
            }
        }

        deps.into_boxed_slice()
    }

    pub(crate) fn group_scope(&self, keys: &[String]) -> Arc<[LoadedDylib]> {
        let mut scope = Vec::with_capacity(keys.len());
        for key in keys {
            let lib = self.get(key).expect("Group library must be registered");
            scope.push(lib.dylib());
        }
        Arc::from(scope)
    }

    pub(crate) fn relocation_scope(
        &self,
        group_scope: &[LoadedDylib],
        flags: OpenFlags,
    ) -> Arc<[LoadedDylib]> {
        let mut seen = BTreeSet::new();
        let mut scope = Vec::with_capacity(group_scope.len() + self.global.len());
        let mut push_unique = |lib: &LoadedDylib| {
            if seen.insert(lib.shortname().to_owned()) {
                scope.push(lib.clone());
            }
        };

        if flags.is_deepbind() {
            for lib in group_scope {
                push_unique(lib);
            }
            for lib in self.global_values() {
                push_unique(lib);
            }
        } else {
            for lib in self.global_values() {
                push_unique(lib);
            }
            for lib in group_scope {
                push_unique(lib);
            }
        }

        Arc::from(scope)
    }

    pub(crate) fn ensure_all_deps(&mut self) {
        let names = self.all.keys().cloned().collect::<Vec<_>>();
        for name in &names {
            let _ = self.ensure_deps(name);
        }
    }

    pub(crate) fn ensure_deps(&mut self, name: &str) -> Option<Arc<[LoadedDylib]>> {
        let canonical = self.canonical_name_owned(name)?;
        if let Some(deps) = self.get(&canonical)?.deps.clone() {
            return Some(deps);
        }

        let deps = build_dependency_scope(self, &canonical);
        self.cache_deps(&canonical, deps.clone());
        Some(deps)
    }

    pub(crate) fn open_existing(&mut self, name: &str, flags: OpenFlags) -> Option<ElfLibrary> {
        let canonical = self.promoted_name(name, flags)?;
        self.get_lib(&canonical)
    }

    pub(crate) fn loaded_existing(
        &mut self,
        name: &str,
        flags: OpenFlags,
    ) -> Option<(LoadedDylib, Box<[String]>)> {
        let canonical = self.promoted_name(name, flags)?;
        let entry = self.get(&canonical)?;
        let direct_deps = self.canonical_direct_deps(entry.dylib_ref());
        Some((entry.dylib(), direct_deps))
    }

    pub(crate) fn get_lib(&mut self, name: &str) -> Option<ElfLibrary> {
        let canonical = self.canonical_name_owned(name)?;
        let deps = self.ensure_deps(&canonical)?;
        let inner = self.get(&canonical)?.dylib();
        Some(ElfLibrary {
            inner,
            deps: Some(deps),
        })
    }

    pub(crate) fn promote(&mut self, shortname: &str, flags: OpenFlags) {
        let promotable = flags.promotable();
        let entry = self.get_mut(shortname).expect("Library must be registered");
        if !entry.flags.contains(promotable) {
            entry.flags |= promotable;
            if flags.is_global() {
                let core = entry.inner.clone();
                self.add_global(shortname.to_owned(), core);
            }
        }
    }
}

/// The global static instance of the library manager, protected by a readers-writer lock.
pub(crate) static MANAGER: Lazy<RwLock<Manager>> = Lazy::new(|| {
    RwLock::new(Manager {
        all: IndexMap::new(),
        global: IndexMap::new(),
        aliases: HashMap::new(),
        identities: HashMap::new(),
        adds: 0,
        subs: 0,
    })
});

/// Registers a library in the global manager.
///
/// If the library has `RTLD_GLOBAL` set, it's also added to the global search scope.
pub(crate) fn register(
    lib: LoadedDylib,
    flags: OpenFlags,
    manager: &mut Manager,
    state: DylibState,
) {
    let name = lib.name();
    let is_main = name.is_empty();
    let shortname = lib.shortname().to_owned();

    let mut flags = flags;
    if name.contains("libc")
        || name.contains("libpthread")
        || name.contains("libdl")
        || name.contains("libgcc_s")
        || name.contains("ld-linux")
        || name.contains("ld-musl")
    {
        flags |= OpenFlags::RTLD_NODELETE;
    }

    log::debug!(
        "Registering library: [{}] (full path: [{}]) flags: [{:?}]",
        shortname,
        name,
        flags
    );

    manager.add(
        shortname.clone(),
        GlobalDylib {
            state,
            inner: lib.clone(),
            flags,
            deps: None,
            libnames: Vec::new(),
        },
    );

    if let Some(identity) = lib.user_data().file_identity {
        manager.add_identity(identity, &shortname);
    }
    if flags.is_global() || is_main {
        manager.add_global(shortname, lib);
    }
}

/// Finds a symbol in the global search scope.
///
/// Iterates through all libraries registered with `RTLD_GLOBAL` in the order they were loaded.
pub(crate) unsafe fn global_find<'a, T>(name: &str) -> Option<crate::Symbol<'a, T>> {
    lock_read!(MANAGER).global_values().find_map(|lib| unsafe {
        lib.get::<T>(name).map(|sym| {
            log::trace!(
                "Lazy Binding: find symbol [{}] from [{}] in global scope ",
                name,
                lib.name()
            );
            core::mem::transmute(sym)
        })
    })
}

/// Finds the next occurrence of a symbol after the specified address.
pub(crate) unsafe fn next_find<'a, T>(addr: usize, name: &str) -> Option<crate::Symbol<'a, T>> {
    let lock = lock_read!(MANAGER);
    // Find the library containing the address
    let (idx, _) = lock.all_iter().enumerate().find(|(_, (_, v))| {
        let start = v.inner.base();
        let end = start + v.inner.mapped_len();
        (start..end).contains(&addr)
    })?;

    // Search in all subsequent libraries
    lock.all_values().skip(idx + 1).find_map(|lib| unsafe {
        lib.inner.get::<T>(name).map(|sym| {
            log::trace!(
                "dlsym: find symbol [{}] from [{}] via RTLD_NEXT",
                name,
                lib.inner.name()
            );
            core::mem::transmute(sym)
        })
    })
}

pub(crate) fn addr2dso(addr: usize) -> Option<ElfLibrary> {
    log::trace!("addr2dso: addr [{:#x}]", addr);
    let manager = crate::lock_read!(MANAGER);
    let entry = manager.all_values().find(|v| {
        let start = v.dylib_ref().base();
        let end = start + v.dylib_ref().mapped_len();
        (start..end).contains(&addr)
    })?;
    let deps = entry
        .deps
        .clone()
        .unwrap_or_else(|| build_dependency_scope(&manager, entry.shortname()));
    Some(ElfLibrary {
        inner: entry.dylib(),
        deps: Some(deps),
    })
}

fn build_dependency_scope(manager: &Manager, root_name: &str) -> Arc<[LoadedDylib]> {
    let mut scope = Vec::new();
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();

    visited.insert(root_name.to_owned());
    queue.push_back(root_name.to_owned());

    while let Some(current_name) = queue.pop_front() {
        let Some(lib_entry) = manager.get(&current_name) else {
            continue;
        };
        let dylib = lib_entry.dylib();
        scope.push(dylib.clone());

        for needed in dylib.needed_libs() {
            let Some(dep) = manager.get(needed) else {
                continue;
            };
            let shortname = dep.shortname().to_owned();
            if visited.insert(shortname.clone()) {
                queue.push_back(shortname);
            }
        }
    }

    Arc::from(scope)
}

pub(crate) fn register_atexit(
    dso_handle: *mut c_void,
    func: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
) -> c_int {
    DESTRUCTORS.write().push(Destructor {
        dso_handle,
        func,
        arg,
    });
    0
}

struct Destructor {
    dso_handle: *mut c_void,
    func: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
}

unsafe impl Send for Destructor {}
unsafe impl Sync for Destructor {}

static DESTRUCTORS: Lazy<RwLock<Vec<Destructor>>> = Lazy::new(|| RwLock::new(Vec::new()));

pub(crate) fn finalize(dso_handle: *mut c_void, range: Option<core::ops::Range<usize>>) {
    let mut to_run = Vec::new();
    {
        let mut range = range;
        if range.is_none() && !dso_handle.is_null() {
            let manager = MANAGER.read();
            for v in manager.all_values() {
                let start = v.dylib_ref().base();
                let end = start + v.dylib_ref().mapped_len();
                if (start..end).contains(&(dso_handle as usize)) {
                    range = Some(start..end);
                    break;
                }
            }
        }

        let mut all_destructors = DESTRUCTORS.write();
        let mut i = 0;
        while i < all_destructors.len() {
            let matches = match (dso_handle.is_null(), &range) {
                (true, _) => true, // NULL matches all
                (false, Some(r)) => r.contains(&(all_destructors[i].dso_handle as usize)),
                (false, None) => all_destructors[i].dso_handle == dso_handle,
            };

            if matches {
                to_run.push(all_destructors.remove(i));
            } else {
                i += 1;
            }
        }
    }
    if !to_run.is_empty() {
        log::debug!(
            "Running {} destructors for handle {:p}",
            to_run.len(),
            dso_handle
        );
    }
    for destructor in to_run.into_iter().rev() {
        unsafe { (destructor.func)(destructor.arg) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_thread_atexit_impl(
    func: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
    dso_handle: *mut c_void,
) -> c_int {
    register_atexit(dso_handle, func, arg)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_atexit(
    func: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
    dso_handle: *mut c_void,
) -> c_int {
    register_atexit(dso_handle, func, arg)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_finalize(dso_handle: *mut c_void) {
    finalize(dso_handle, None);
}
