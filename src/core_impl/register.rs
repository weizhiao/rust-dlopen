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
        if self.flags.contains(OpenFlags::RTLD_NODELETE)
            | self.flags.contains(OpenFlags::CUSTOM_NOT_REGISTER)
        {
            return;
        }
        let mut removed_libs = Vec::new();
        {
            let mut lock = lock_write!(MANAGER);
            let ref_count = unsafe { self.inner.core_ref().strong_count() };
            // Dylib ref + deps ref (if present) + global ref (if present)
            // Note: implicit refs such as from loader internal usage?
            let threshold = 2
                + self.deps.is_some() as usize
                + self.flags.contains(OpenFlags::RTLD_GLOBAL) as usize;

            if ref_count == threshold {
                log::info!("Destroying dylib [{}]", self.inner.name());
                removed_libs.push(self.inner.clone());
                let shortname = self.inner.shortname();

                lock.all.shift_remove(shortname);
                lock.subs += 1;
                if self.flags.contains(OpenFlags::RTLD_GLOBAL) {
                    lock.global.shift_remove(shortname);
                }

                // Check dependencies
                if let Some(deps) = self.deps.as_ref() {
                    for dep in deps.iter().skip(1) {
                        let dep_shortname = dep.shortname();
                        let dep_threshold = if let Some(lib) = lock.all.get(dep_shortname) {
                            if lib.flags.contains(OpenFlags::RTLD_NODELETE) {
                                continue;
                            }
                            2 + lib.deps.is_some() as usize
                                + lib.flags.contains(OpenFlags::RTLD_GLOBAL) as usize
                        } else {
                            continue;
                        };

                        if unsafe { dep.core_ref().strong_count() } == dep_threshold {
                            log::info!("Destroying dylib [{}]", dep.name());
                            removed_libs.push(dep.clone());
                            lock.all.shift_remove(dep_shortname);
                            lock.subs += 1;
                            // Bug fix: remove dep from global, not self!
                            lock.global.shift_remove(dep_shortname);
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
    flags: OpenFlags,
    /// The full dependency scope (Searchlist) used for symbol resolution,
    /// calculated via BFS starting from this library.
    deps: Option<Arc<[LoadedDylib]>>,
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
            flags: self.flags,
            deps: self.deps.clone(),
        }
    }

    #[inline]
    pub(crate) fn set_flags(&mut self, flags: OpenFlags) {
        self.flags = flags;
    }

    #[inline]
    pub(crate) fn flags(&self) -> OpenFlags {
        self.flags
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
    pub(crate) all: IndexMap<String, GlobalDylib>,
    /// Libraries available in the global symbol scope (RTLD_GLOBAL).
    pub(crate) global: IndexMap<String, LoadedDylib>,
    /// The number of times a new object has been added to the link map.
    pub(crate) adds: u64,
    /// The number of times an object has been removed from the link map.
    pub(crate) subs: u64,
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
    if flags.contains(OpenFlags::CUSTOM_NOT_REGISTER) {
        log::trace!(
            "Skipping registration for [{}] due to CUSTOM_NOT_REGISTER",
            lib.name()
        );
        return;
    }
    let name = lib.name();
    let is_main = name.is_empty();
    let shortname = lib.shortname().to_owned();

    let mut flags = flags;
    if shortname.contains("libc.so")
        || shortname.contains("libpthread.so")
        || shortname.contains("libdl.so")
        || shortname.contains("libgcc_s.so")
        || shortname.contains("ld-linux")
    {
        flags |= OpenFlags::RTLD_NODELETE;
    }

    log::debug!(
        "Registering library: [{}] (full path: [{}]) flags: [{:?}]",
        shortname,
        name,
        flags
    );

    manager.all.insert(
        shortname.clone(),
        GlobalDylib {
            state,
            inner: lib.clone(),
            flags,
            deps: None,
        },
    );
    manager.adds += 1;
    log::trace!("Registered [{}] in all map", shortname);
    if flags.contains(OpenFlags::RTLD_GLOBAL) || is_main {
        log::trace!("Adding [{}] to global scope", shortname);
        manager.global.insert(shortname, lib);
    }
}

/// Finds a symbol in the global search scope.
///
/// Iterates through all libraries registered with `RTLD_GLOBAL` in the order they were loaded.
pub(crate) fn global_find(name: &str) -> Option<*const ()> {
    lock_read!(MANAGER).global.values().find_map(|lib| unsafe {
        lib.get::<()>(name).map(|sym| {
            log::trace!(
                "Lazy Binding: find symbol [{}] from [{}] in global scope ",
                name,
                lib.name()
            );
            let val = sym.into_raw();
            assert!(lib.base() != val as usize);
            val
        })
    })
}

/// Updates the dependency Searchlist for the specified root libraries.
///
/// For each root, it performs a Breadth-First Search (BFS) over its dependency tree
/// to calculate a flat list of all libraries (the Searchlist) used for symbol resolution.
/// This matches the behavior of the glibc dynamic linker.
pub(crate) fn update_dependency_scopes(manager: &mut Manager, roots: &[String]) {
    for root_name in roots {
        debug_assert!(
            manager.all.contains_key(root_name),
            "Root library [{}] must be registered",
            root_name
        );

        let root_lib = &manager.all[root_name];

        debug_assert!(
            root_lib.deps.is_none(),
            "Dependency scope for root library [{}] is already set",
            root_name
        );

        let mut scope = Vec::new();
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();

        visited.insert(root_name.clone());
        queue.push_back(root_name.clone());

        while let Some(current_name) = queue.pop_front() {
            let lib_entry = &manager.all[&current_name];
            let dylib = lib_entry.dylib();
            scope.push(dylib.clone());

            for needed in dylib.needed_libs() {
                debug_assert!(
                    manager.all.contains_key(needed),
                    "Dependency [{}] of [{}] must be registered",
                    needed,
                    current_name
                );
                if !visited.contains(needed) {
                    visited.insert(needed.to_owned());
                    queue.push_back(needed.to_owned());
                }
            }
        }
        manager.all[root_name].set_deps(Arc::from(scope));
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
            for v in manager.all.values() {
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
