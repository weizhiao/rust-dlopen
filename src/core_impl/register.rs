use crate::{
    ElfLibrary, OpenFlags,
    core_impl::loader::{DylibExt, LoadedDylib},
};
use alloc::{
    borrow::ToOwned,
    collections::{btree_set::BTreeSet, vec_deque::VecDeque},
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::ffi::{c_int, c_void};
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
///
/// The state uses a compact u8 representation:
/// - `[0, 254)`: Newly loaded library, value is the index in the current loading batch.
/// - `254`: Currently undergoing relocation.
/// - `255`: Successfully relocated and ready for use.
#[derive(Clone, Copy, Default)]
pub(crate) struct DylibState(u8);

impl DylibState {
    const RELOCATED: u8 = 0b11111111;
    const RELOCATING: u8 = 0b11111110;

    /// Returns true if the library is fully relocated.
    #[inline]
    pub(crate) fn is_relocated(&self) -> bool {
        self.0 == Self::RELOCATED
    }

    /// If the library is in a "new" state, returns its batch index.
    #[inline]
    pub(crate) fn get_new_idx(&self) -> Option<u8> {
        if self.0 >= Self::RELOCATING {
            None
        } else {
            Some(self.0)
        }
    }

    /// Transitions the state to Relocated.
    #[inline]
    pub(crate) fn set_relocated(&mut self) -> &mut Self {
        self.0 = Self::RELOCATED;
        self
    }

    /// Transitions the state to Relocating.
    #[inline]
    pub(crate) fn set_relocating(&mut self) -> &mut Self {
        self.0 = Self::RELOCATING;
        self
    }

    /// Sets the state to a "new" library with the given batch index.
    #[allow(unused)]
    #[inline]
    pub(crate) fn set_new_idx(&mut self, idx: u8) -> &mut Self {
        assert!(idx < Self::RELOCATING);
        self.0 = idx;
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
}

unsafe impl Send for GlobalDylib {}
unsafe impl Sync for GlobalDylib {}

impl GlobalDylib {
    #[inline]
    pub(crate) fn get_lib(&self) -> ElfLibrary {
        debug_assert!(self.deps.is_some());
        ElfLibrary {
            inner: self.inner.clone(),
            deps: self.deps.clone(),
        }
    }

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

    #[inline]
    pub(crate) fn set_deps(&mut self, deps: Arc<[LoadedDylib]>) {
        self.deps = Some(deps);
    }
}

/// The global manager for all loaded dynamic libraries.
pub(crate) struct Manager {
    /// Maps a library's short name (e.g., "libc.so.6") to its full metadata.
    /// Uses `IndexMap` to preserve loading order for symbol resolution.
    all: IndexMap<String, GlobalDylib>,
    /// Libraries available in the global symbol scope (RTLD_GLOBAL).
    global: IndexMap<String, LoadedDylib>,
    /// The number of times a new object has been added to the link map.
    adds: u64,
    /// The number of times an object has been removed from the link map.
    subs: u64,
}

impl Manager {
    pub(crate) fn add_global(&mut self, name: String, lib: LoadedDylib) {
        debug_assert!(
            !self.global.contains_key(&name),
            "Library [{}] is already in global scope",
            name
        );
        log::trace!("Adding [{}] to global scope", name);
        self.global.insert(name, lib);
    }

    pub(crate) fn add(&mut self, name: String, lib: GlobalDylib) {
        let res = self.all.insert(name.clone(), lib);
        debug_assert!(res.is_none(), "Library [{}] is already registered", name);
        self.adds += 1;
        log::trace!("Registered [{}] in all map", name);
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
    }

    #[inline]
    pub(crate) fn get(&self, name: &str) -> Option<&GlobalDylib> {
        self.all.get(name)
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut GlobalDylib> {
        self.all.get_mut(name)
    }

    #[inline]
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.all.contains_key(name)
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
    pub(crate) fn keys(&self) -> indexmap::map::Keys<'_, String, GlobalDylib> {
        self.all.keys()
    }

    #[inline]
    pub(crate) fn get_index(&self, index: usize) -> Option<(&String, &GlobalDylib)> {
        self.all.get_index(index)
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
        },
    );
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
    // Use the manager directly to avoid potential cloning if not needed,
    // but here we return ElfLibrary which is a wrapper.
    crate::lock_read!(MANAGER).all_values().find_map(|v| {
        let start = v.dylib_ref().base();
        let end = start + v.dylib_ref().mapped_len();
        if (start..end).contains(&addr) {
            Some(v.get_lib())
        } else {
            None
        }
    })
}

/// Updates the dependency Searchlist for the specified root libraries.
///
/// For each root, it performs a Breadth-First Search (BFS) over its dependency tree
/// to calculate a flat list of all libraries (the Searchlist) used for symbol resolution.
/// This matches the behavior of the glibc dynamic linker.
pub(crate) fn update_dependency_scopes<'a>(
    manager: &mut Manager,
    roots: impl Iterator<Item = &'a str>,
) {
    for root_name in roots {
        let Some(root_lib) = manager.get(root_name) else {
            continue;
        };

        if root_lib.deps.is_some() {
            continue;
        }

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
                if !manager.contains(needed) {
                    continue;
                }
                if !visited.contains(needed) {
                    visited.insert(needed.to_owned());
                    queue.push_back(needed.to_owned());
                }
            }
        }
        if let Some(entry) = manager.get_mut(root_name) {
            entry.set_deps(Arc::from(scope));
        }
    }
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
