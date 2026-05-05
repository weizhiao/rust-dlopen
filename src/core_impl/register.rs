use super::{
    loader::{DylibExt, LoadedDylib},
    types::{ExtraData, FileIdentity},
};
use crate::{ElfLibrary, OpenFlags};
use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::btree_set::BTreeSet,
    string::String,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::ffi::{c_int, c_void};
use elf_loader::linker::LinkContext;
use hashbrown::{DefaultHashBuilder, HashMap};
use spin::{Lazy, RwLock};

type IndexMap<K, V> = indexmap::IndexMap<K, V, DefaultHashBuilder>;

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
            let Some(flags) = lock.flags(shortname) else {
                return;
            };

            if flags.is_nodelete() {
                return;
            }

            let ref_count = unsafe { self.inner.core_ref().strong_count() };
            let has_global = flags.is_global();
            // Dylib ref in committed link_ctx + dylib ref in this handle's deps list
            // + global ref (if present) + handle's inner ref.
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
                        let Some(dep_flags) = lock.flags(dep_shortname) else {
                            continue;
                        };
                        if dep_flags.is_nodelete() {
                            continue;
                        }
                        // Dylib ref in committed link_ctx + dylib ref in the owner deps list
                        // + global ref (if present).
                        let dep_threshold = 3 + dep_flags.is_global() as usize;

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

#[derive(Clone)]
pub(crate) struct PendingDylib {
    inner: LoadedDylib,
    pub(crate) flags: OpenFlags,
    pub(crate) libnames: Vec<String>,
}

unsafe impl Send for PendingDylib {}
unsafe impl Sync for PendingDylib {}

impl PendingDylib {
    fn new(inner: LoadedDylib, flags: OpenFlags) -> Self {
        Self {
            inner,
            flags,
            libnames: Vec::new(),
        }
    }

    #[inline]
    fn dylib(&self) -> LoadedDylib {
        self.inner.clone()
    }

    #[inline]
    fn dylib_ref(&self) -> &LoadedDylib {
        &self.inner
    }
}

#[derive(Clone)]
pub(crate) enum LibraryLookup<'a> {
    Pending {
        shortname: Cow<'a, str>,
    },
    Relocated {
        shortname: Cow<'a, str>,
        name: Cow<'a, str>,
    },
}

impl<'a> LibraryLookup<'a> {
    pub(crate) fn is_relocated(&self) -> bool {
        matches!(self, Self::Relocated { .. })
    }

    pub(crate) fn shortname(&self) -> &str {
        match self {
            Self::Pending { shortname } | Self::Relocated { shortname, .. } => shortname,
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            Self::Relocated { name, .. } => Some(name),
            Self::Pending { .. } => None,
        }
    }

    pub(crate) fn into_owned(self) -> LibraryLookup<'static> {
        match self {
            Self::Pending { shortname } => LibraryLookup::Pending {
                shortname: Cow::Owned(shortname.into_owned()),
            },
            Self::Relocated { shortname, name } => LibraryLookup::Relocated {
                shortname: Cow::Owned(shortname.into_owned()),
                name: Cow::Owned(name.into_owned()),
            },
        }
    }

    pub(crate) fn into_shortname_owned(self) -> String {
        match self {
            Self::Pending { shortname } | Self::Relocated { shortname, .. } => {
                shortname.into_owned()
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct GlobalMeta {
    pub(crate) flags: OpenFlags,
    pub(crate) libnames: Vec<String>,
}

impl Default for GlobalMeta {
    #[inline]
    fn default() -> Self {
        Self {
            flags: OpenFlags::empty(),
            libnames: Vec::new(),
        }
    }
}

/// The global manager for all loaded dynamic libraries.
pub(crate) struct Manager {
    /// Libraries that are visible to concurrent `dlopen` calls but are not yet
    /// committed to the dependency graph.
    pending: IndexMap<String, PendingDylib>,
    /// Libraries available in the global symbol scope (RTLD_GLOBAL).
    global: IndexMap<String, LoadedDylib>,
    /// Alias names that resolve to a canonical short name.
    aliases: HashMap<String, String>,
    /// Maps file identities to the canonical short name for fast inode-based lookup.
    identities: HashMap<FileIdentity, String>,
    /// Fully linked modules indexed by canonical key.
    link_ctx: LinkContext<String, ExtraData, GlobalMeta>,
    /// The number of times a new object has been added to the link map.
    adds: u64,
    /// The number of times an object has been removed from the link map.
    subs: u64,
}

impl Manager {
    fn contains_canonical_key(&self, key: &str) -> bool {
        self.link_ctx.contains_key(key) || self.pending.contains_key(key)
    }

    fn committed_lookup<'a>(&'a self, key: &str) -> Option<LibraryLookup<'a>> {
        let (shortname, inner) = self.link_ctx.get_key_value(key)?;
        Some(LibraryLookup::Relocated {
            shortname: Cow::Borrowed(shortname),
            name: Cow::Borrowed(inner.name()),
        })
    }

    fn pending_lookup<'a>(&'a self, key: &str) -> Option<LibraryLookup<'a>> {
        self.pending
            .get_key_value(key)
            .map(|(shortname, _)| LibraryLookup::Pending {
                shortname: Cow::Borrowed(shortname),
            })
    }

    fn canonical_name_owned(&self, name: &str) -> Option<String> {
        Some(self.lookup(name)?.shortname().to_owned())
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

    fn add_loaded(&mut self, name: String, lib: LoadedDylib, flags: OpenFlags) {
        debug_assert!(
            !self.contains_canonical_key(&name),
            "Library [{}] is already registered",
            name
        );
        let direct_deps = self.canonical_direct_deps(&lib);
        self.link_ctx
            .insert_with_meta(
                name.clone(),
                lib,
                direct_deps,
                GlobalMeta {
                    flags,
                    libnames: Vec::new(),
                },
            )
            .expect("registry insert must not insert duplicate keys");
        self.adds += 1;
        log::trace!("Registered [{}] in global manager", name);
    }

    fn add_pending(&mut self, name: String, lib: LoadedDylib, flags: OpenFlags) {
        debug_assert!(
            !self.contains_canonical_key(&name),
            "Library [{}] is already registered",
            name
        );
        let previous = self
            .pending
            .insert(name.clone(), PendingDylib::new(lib, flags));
        debug_assert!(previous.is_none(), "Library [{}] is already pending", name);
        self.adds += 1;
        log::trace!("Registered [{}] in global manager", name);
    }

    pub(crate) fn add_alias(&mut self, canonical: &str, alias: &str) {
        debug_assert!(
            self.contains_canonical_key(canonical),
            "Canonical library [{}] must be registered before adding aliases",
            canonical
        );

        if self.contains_canonical_key(alias) {
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

        log::trace!("Adding alias [{}] to library [{}]", alias, canonical);
        if let Some(lib) = self.pending.get_mut(canonical) {
            lib.libnames.push(alias.to_owned());
        } else {
            self.link_ctx
                .meta_mut(canonical)
                .expect("Canonical library must be registered before adding aliases")
                .libnames
                .push(alias.to_owned());
        }
        self.aliases.insert(alias.to_owned(), canonical.to_owned());
    }

    pub(crate) fn add_identity(&mut self, identity: FileIdentity, name: &str) {
        // Newest wins; identical inode implies same physical file.
        self.identities.insert(identity, name.to_owned());
    }

    pub(crate) fn remove(&mut self, shortname: &str) {
        let removed = if let Some(lib) = self.pending.shift_remove(shortname) {
            Some((false, lib.flags, lib.libnames))
        } else {
            self.link_ctx
                .remove(shortname)
                .map(|(_, _, meta)| (true, meta.flags, meta.libnames))
        };
        let Some((was_committed, flags, libnames)) = removed else {
            panic!("Library is not registered");
        };
        self.subs += 1;
        let res = self.global.shift_remove(shortname);
        debug_assert!(
            !was_committed || flags.is_global() == res.is_some(),
            "Inconsistent global scope state when removing [{}]",
            shortname
        );
        for alias in &libnames {
            self.aliases.remove(alias);
        }
        // Remove any identity aliases pointing to this shortname.
        self.identities.retain(|_, v| v != shortname);
    }

    #[inline]
    pub(crate) fn lookup<'a>(&'a self, name: &str) -> Option<LibraryLookup<'a>> {
        // Primary lookup by canonical shortname.
        if let Some(lib) = self.committed_lookup(name) {
            return Some(lib);
        }
        if let Some(lib) = self.pending_lookup(name) {
            return Some(lib);
        }
        let canonical = self.aliases.get(name)?;
        self.committed_lookup(canonical)
            .or_else(|| self.pending_lookup(canonical))
    }

    pub(crate) fn flags(&self, name: &str) -> Option<OpenFlags> {
        if let Some(meta) = self.link_ctx.meta(name) {
            return Some(meta.flags);
        }
        if let Some(lib) = self.pending.get(name) {
            return Some(lib.flags);
        }
        let canonical = self.aliases.get(name)?;
        self.link_ctx
            .meta(canonical)
            .map(|meta| meta.flags)
            .or_else(|| self.pending.get(canonical).map(|lib| lib.flags))
    }

    #[inline]
    pub(crate) fn all_values(&self) -> impl Iterator<Item = LoadedDylib> + '_ {
        self.link_ctx
            .load_order()
            .iter()
            .filter_map(|key| self.link_ctx.get(key).cloned())
    }

    #[inline]
    pub(crate) fn global_values(&self) -> indexmap::map::Values<'_, String, LoadedDylib> {
        self.global.values()
    }

    pub(crate) fn adds(&self) -> u64 {
        self.adds
    }

    pub(crate) fn subs(&self) -> u64 {
        self.subs
    }

    #[inline]
    pub(crate) fn lookup_by_identity<'a>(
        &'a self,
        identity: &FileIdentity,
    ) -> Option<LibraryLookup<'a>> {
        self.identities
            .get(identity)
            .and_then(|name| self.lookup(name))
    }

    #[inline]
    pub(crate) fn main_library(&self) -> Option<ElfLibrary> {
        let key = self.link_ctx.load_order().first()?;
        let lib = self.link_ctx.get(key)?.clone();
        let deps = self.library_scope(key)?;
        Some(ElfLibrary {
            inner: lib,
            deps: Some(deps),
        })
    }

    pub(crate) fn canonical_direct_deps(&self, lib: &LoadedDylib) -> Box<[String]> {
        let mut deps = Vec::with_capacity(lib.needed_libs().len());
        let mut seen = BTreeSet::new();

        for needed in lib.needed_libs() {
            let Some(dep) = self.lookup(needed) else {
                continue;
            };
            let shortname = dep.shortname().to_owned();
            if seen.insert(shortname.clone()) {
                deps.push(shortname);
            }
        }

        deps.into_boxed_slice()
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

    #[allow(unused)]
    pub(crate) fn rebuild_link_ctx(&mut self) {
        let entries = self
            .link_ctx
            .load_order()
            .iter()
            .map(|key| {
                let module = self
                    .link_ctx
                    .get(key)
                    .cloned()
                    .expect("load_order entries must resolve to committed modules");
                let meta = self
                    .link_ctx
                    .meta(key)
                    .cloned()
                    .expect("load_order entries must resolve to committed metadata");
                let direct_deps = self.canonical_direct_deps(&module);
                (key.clone(), module, direct_deps, meta)
            })
            .collect::<Vec<_>>();

        self.link_ctx = LinkContext::new();
        for (key, module, direct_deps, meta) in entries {
            self.link_ctx
                .insert_with_meta(key, module, direct_deps, meta)
                .expect("registry rebuild must not insert duplicate keys");
        }
    }

    pub(crate) fn merge_link_context(
        &mut self,
        source: &LinkContext<String, ExtraData, GlobalMeta>,
        keys: impl IntoIterator<Item = String>,
    ) {
        for key in keys {
            if self.link_ctx.contains_key(&key) {
                continue;
            }

            let Some(module) = source.get(&key).cloned() else {
                continue;
            };
            let direct_deps = source
                .direct_deps(&key)
                .unwrap_or(&[])
                .to_vec()
                .into_boxed_slice();
            let pending = self.pending.shift_remove(&key);
            let meta = pending
                .as_ref()
                .map(|lib| GlobalMeta {
                    flags: lib.flags,
                    libnames: lib.libnames.clone(),
                })
                .unwrap_or_default();
            self.link_ctx
                .insert_with_meta(key.clone(), module.clone(), direct_deps, meta.clone())
                .expect("load merge must not insert duplicate keys");
            if let Some(identity) = module.user_data().file_identity {
                self.add_identity(identity, &key);
            }
            if meta.flags.is_global() {
                self.add_global(key, module);
            }
        }
    }

    pub(crate) fn visible_contains(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    pub(crate) fn visible_direct_deps(&self, name: &str) -> Option<Box<[String]>> {
        let canonical = self.canonical_name_owned(name)?;
        if let Some(direct_deps) = self.link_ctx.direct_deps(&canonical) {
            return Some(direct_deps.to_vec().into_boxed_slice());
        }

        let lib = self.pending.get(&canonical)?;
        Some(self.canonical_direct_deps(lib.dylib_ref()))
    }

    pub(crate) fn visible_loaded(&self, name: &str) -> Option<LoadedDylib> {
        let canonical = self.canonical_name_owned(name)?;
        self.link_ctx
            .get(&canonical)
            .cloned()
            .or_else(|| self.pending.get(&canonical).map(PendingDylib::dylib))
    }

    pub(crate) fn open_existing(&mut self, name: &str, flags: OpenFlags) -> Option<ElfLibrary> {
        let canonical = self.promoted_name(name, flags)?;
        self.get_lib(&canonical)
    }

    pub(crate) fn get_lib(&mut self, name: &str) -> Option<ElfLibrary> {
        let canonical = self.canonical_name_owned(name)?;
        let deps = self.library_scope(&canonical)?;
        let inner = self.link_ctx.get(&canonical).cloned()?;
        Some(ElfLibrary {
            inner,
            deps: Some(deps),
        })
    }

    pub(crate) fn promote(&mut self, shortname: &str, flags: OpenFlags) {
        let promotable = flags.promotable();
        let entry = self
            .link_ctx
            .meta_mut(shortname)
            .expect("Library must be registered");
        if !entry.flags.contains(promotable) {
            entry.flags |= promotable;
            if flags.is_global() {
                let core = self
                    .link_ctx
                    .get(shortname)
                    .cloned()
                    .expect("Promoted library must be committed");
                let key = shortname.to_owned();
                self.add_global(key, core);
            }
        }
    }

    pub(crate) fn library_scope(&self, name: &str) -> Option<Arc<[LoadedDylib]>> {
        let canonical = self.canonical_name_owned(name)?;
        let deps = self.link_ctx.dependency_scope(&canonical);
        if !deps.is_empty() {
            return Some(deps);
        }

        let entry = self.link_ctx.get(&canonical)?;
        Some(Arc::from(vec![entry.clone()]))
    }
}

/// The global static instance of the library manager, protected by a readers-writer lock.
pub(crate) static MANAGER: Lazy<RwLock<Manager>> = Lazy::new(|| {
    RwLock::new(Manager {
        pending: IndexMap::with_hasher(DefaultHashBuilder::default()),
        global: IndexMap::with_hasher(DefaultHashBuilder::default()),
        aliases: HashMap::new(),
        identities: HashMap::new(),
        link_ctx: LinkContext::new(),
        adds: 0,
        subs: 0,
    })
});

fn normalized_flags(name: &str, mut flags: OpenFlags) -> OpenFlags {
    if name.contains("libc")
        || name.contains("libpthread")
        || name.contains("libdl")
        || name.contains("libgcc_s")
        || name.contains("ld-linux")
        || name.contains("ld-musl")
    {
        flags |= OpenFlags::RTLD_NODELETE;
    }
    flags
}

fn libc_compat_aliases(shortname: &str) -> &'static [&'static str] {
    match shortname {
        "libc.so.6" => &[
            "libdl.so.2",
            "libpthread.so.0",
            "libutil.so.1",
            "librt.so.1",
            "libanl.so.1",
        ],
        "ld-linux-x86-64.so.2" => &["ld-linux.so.2"],
        _ => &[],
    }
}

pub(crate) fn register_pending(
    lib: LoadedDylib,
    flags: OpenFlags,
    manager: &mut Manager,
) -> String {
    let name = lib.name();
    let shortname = lib.shortname().to_owned();
    let flags = normalized_flags(name, flags);

    log::debug!(
        "Registering pending library: [{}] (full path: [{}]) flags: [{:?}]",
        shortname,
        name,
        flags
    );

    manager.add_pending(shortname.clone(), lib.clone(), flags);

    if let Some(identity) = lib.user_data().file_identity {
        manager.add_identity(identity, &shortname);
    }
    for alias in libc_compat_aliases(&shortname) {
        manager.add_alias(&shortname, alias);
    }

    shortname
}

/// Registers a relocated library in the global manager.
///
/// If the library has `RTLD_GLOBAL` set, it's also added to the global search scope.
pub(crate) fn register_loaded(lib: LoadedDylib, flags: OpenFlags, manager: &mut Manager) {
    let name = lib.name();
    let is_main = name.is_empty();
    let shortname = lib.shortname().to_owned();
    let flags = normalized_flags(name, flags);

    log::debug!(
        "Registering loaded library: [{}] (full path: [{}]) flags: [{:?}]",
        shortname,
        name,
        flags
    );

    manager.add_loaded(shortname.clone(), lib.clone(), flags);

    if let Some(identity) = lib.user_data().file_identity {
        manager.add_identity(identity, &shortname);
    }
    for alias in libc_compat_aliases(&shortname) {
        manager.add_alias(&shortname, alias);
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
    let libs = lock.all_values().collect::<Vec<_>>();
    // Find the library containing the address
    let idx = libs.iter().position(|v| {
        let start = v.base();
        let end = start + v.mapped_len();
        (start..end).contains(&addr)
    })?;

    // Search in all subsequent libraries
    libs.into_iter().skip(idx + 1).find_map(|lib| unsafe {
        lib.get::<T>(name).map(|sym| {
            log::trace!(
                "dlsym: find symbol [{}] from [{}] via RTLD_NEXT",
                name,
                lib.name()
            );
            core::mem::transmute(sym)
        })
    })
}

pub(crate) fn addr2dso(addr: usize) -> Option<ElfLibrary> {
    log::trace!("addr2dso: addr [{:#x}]", addr);
    let manager = crate::lock_read!(MANAGER);
    let entry = manager.all_values().find(|v| {
        let start = v.base();
        let end = start + v.mapped_len();
        (start..end).contains(&addr)
    })?;
    let deps = manager.library_scope(entry.shortname())?;
    Some(ElfLibrary {
        inner: entry,
        deps: Some(deps),
    })
}

fn register_atexit(
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

fn finalize(dso_handle: *mut c_void, range: Option<core::ops::Range<usize>>) {
    let mut to_run = Vec::new();
    {
        let mut range = range;
        if range.is_none() && !dso_handle.is_null() {
            let manager = MANAGER.read();
            for v in manager.all_values() {
                let start = v.base();
                let end = start + v.mapped_len();
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
