use crate::{
    ElfLibrary, OpenFlags,
    core_impl::loader::{DylibExt, RelocatedDylib},
};
use alloc::{
    borrow::ToOwned,
    collections::{btree_set::BTreeSet, vec_deque::VecDeque},
    string::String,
    sync::Arc,
    vec::Vec,
};
use indexmap::IndexMap;
use spin::{Lazy, RwLock};

#[macro_export]
macro_rules! lock_write {
    ($lock:expr) => {{
        log::trace!("LOCK_WRITE ATTEMPT: {}:{}", file!(), line!());
        let guard = $lock.write();
        log::trace!("LOCK_WRITE ACQUIRED: {}:{}", file!(), line!());
        guard
    }};
}

#[macro_export]
macro_rules! lock_read {
    ($lock:expr) => {{
        log::trace!("LOCK_READ ATTEMPT: {}:{}", file!(), line!());
        let guard = $lock.read();
        log::trace!("LOCK_READ ACQUIRED: {}:{}", file!(), line!());
        guard
    }};
}

impl Drop for ElfLibrary {
    fn drop(&mut self) {
        if self.flags.contains(OpenFlags::RTLD_NODELETE)
            | self.flags.contains(OpenFlags::CUSTOM_NOT_REGISTER)
        {
            return;
        }
        let mut lock = lock_write!(MANAGER);
        let ref_count = unsafe { self.inner.core_ref().strong_count() };
        // Dylib ref + deps ref (if present) + global ref (if present)
        // Note: implicit refs such as from loader internal usage?
        let threshold =
            2 + self.deps.is_some() as usize + self.flags.contains(OpenFlags::RTLD_GLOBAL) as usize;

        if ref_count == threshold {
            log::info!("Destroying dylib [{}]", self.inner.name());
            let shortname = self.inner.shortname();

            lock.all.shift_remove(shortname);
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
                        lock.all.shift_remove(dep_shortname);
                        // Bug fix: remove dep from global, not self!
                        lock.global.shift_remove(dep_shortname);
                    }
                }
            }
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
    inner: RelocatedDylib,
    flags: OpenFlags,
    /// The full dependency scope (Searchlist) used for symbol resolution, 
    /// calculated via BFS starting from this library.
    deps: Option<Arc<[RelocatedDylib]>>,
    /// Current state in the load/relocate lifecycle.
    pub(crate) state: DylibState,
}

unsafe impl Send for GlobalDylib {}
unsafe impl Sync for GlobalDylib {}

impl GlobalDylib {
    #[inline]
    pub(crate) fn get_dylib(&self) -> ElfLibrary {
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
    pub(crate) fn relocated_dylib(&self) -> RelocatedDylib {
        self.inner.clone()
    }

    #[inline]
    pub(crate) fn relocated_dylib_ref(&self) -> &RelocatedDylib {
        &self.inner
    }

    #[inline]
    pub(crate) fn shortname(&self) -> &str {
        self.inner.shortname()
    }

    #[inline]
    pub(crate) fn set_deps(&mut self, deps: Arc<[RelocatedDylib]>) {
        self.deps = Some(deps);
    }
}

/// The global manager for all loaded dynamic libraries.
pub(crate) struct Manager {
    /// Maps a library's short name (e.g., "libc.so.6") to its full metadata.
    /// Uses `IndexMap` to preserve loading order for symbol resolution.
    pub(crate) all: IndexMap<String, GlobalDylib>,
    /// Libraries available in the global symbol scope (RTLD_GLOBAL).
    pub(crate) global: IndexMap<String, RelocatedDylib>,
}

/// The global static instance of the library manager, protected by a readers-writer lock.
pub(crate) static MANAGER: Lazy<RwLock<Manager>> = Lazy::new(|| {
    RwLock::new(Manager {
        all: IndexMap::new(),
        global: IndexMap::new(),
    })
});

/// Registers a library in the global manager.
/// 
/// If the library has `RTLD_GLOBAL` set, it's also added to the global search scope.
pub(crate) fn register(
    lib: RelocatedDylib,
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
        if let Some(entry) = manager.all.get(root_name) {
            if entry.deps.is_some() {
                continue;
            }
        } else {
            continue;
        }

        let mut scope = Vec::new();
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();

        visited.insert(root_name.clone());
        queue.push_back(root_name.clone());

        while let Some(current_name) = queue.pop_front() {
            if let Some(lib_entry) = manager.all.get(&current_name) {
                let dylib = lib_entry.relocated_dylib();
                scope.push(dylib.clone());

                for needed in dylib.needed_libs() {
                    // 尝试在已注册的库中查找
                    let mut found_key = None;
                    if manager.all.contains_key(needed) {
                        found_key = Some(needed.to_owned());
                    } else {
                        // 模糊匹配：通过全路径后缀或名称匹配
                        for (key, val) in &manager.all {
                            if val.relocated_dylib_ref().name().ends_with(needed) || key == needed {
                                found_key = Some(key.clone());
                                break;
                            }
                        }
                    }

                    if let Some(key) = found_key {
                        if !visited.contains(&key) {
                            visited.insert(key.clone());
                            queue.push_back(key);
                        }
                    }
                }
            }
        }

        if let Some(entry) = manager.all.get_mut(root_name) {
            entry.set_deps(Arc::from(scope));
        }
    }
}
