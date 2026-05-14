#[cfg(not(feature = "std"))]
use crate::core_impl::ElfDylib;
#[cfg(not(feature = "std"))]
use crate::core_impl::shortname_from_name;
use crate::{
    OpenFlags, Result,
    core_impl::{
        AsFilename, DylibExt, ENVP, ElfLibrary, ExtraData, GlobalMeta, LibraryLookup, LoadedDylib,
        MANAGER, Manager, new_loader, reserve_pending,
    },
    error::find_lib_error,
    utils::{ld_cache::LdCache, linker_script::get_linker_script_libs},
};
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    collections::BTreeSet,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    cell::RefCell,
    ffi::{CStr, c_char, c_int, c_void},
};
use elf_loader::image::{ModuleHandle, ModuleScope};
use elf_loader::input::{ElfBinary, ElfFile, ElfReader, Path as LoaderPath, PathBuf as ElfPath};
use elf_loader::linker::{
    DependencyRequest, KeyId, KeyResolver, LinkContext, Linker, RelocationInputs,
    RelocationPlanner, RelocationRequest, ResolvedKey, RootRequest, VisibleModules,
};
use spin::{Lazy, RwLockWriteGuard};

fn get_env(name: &str) -> Option<&'static str> {
    unsafe {
        let mut cur = ENVP;
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
            .main_library()
            .expect("Main executable must be initialized")
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
struct OpenShared<'a> {
    /// The write lock guard for the global library manager.
    /// Can be temporarily dropped to avoid deadlocks during relocation.
    lock: RefCell<Option<RwLockWriteGuard<'a, Manager>>>,
    /// Loading flags for this operation.
    flags: OpenFlags,
}

struct OpenContext<'a> {
    shared: OpenShared<'a>,
    /// Names of libraries that were added to the global registry in this operation.
    added_names: BTreeSet<String>,
    /// Indicates if the operation was successfully committed.
    committed: bool,
}

enum LinkRoot<'bytes> {
    Load {
        key: String,
        bytes: Option<&'bytes [u8]>,
    },
    #[cfg(not(feature = "std"))]
    Mapped { key: String, raw: ElfDylib },
}

impl<'bytes> LinkRoot<'bytes> {
    fn bytes(&self) -> Option<&'bytes [u8]> {
        match self {
            Self::Load { bytes, .. } => *bytes,
            #[cfg(not(feature = "std"))]
            Self::Mapped { .. } => None,
        }
    }
}

impl<'a> Drop for OpenContext<'a> {
    fn drop(&mut self) {
        // If not committed, roll back changes to the global registry.
        if !self.committed {
            log::debug!("Destroying newly added dynamic libraries from the global");
            let mut lock = self
                .shared
                .lock
                .borrow_mut()
                .take()
                .unwrap_or_else(|| crate::lock_write!(MANAGER));
            self.remove_added_libraries(&mut lock);
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
            shared: OpenShared {
                lock: RefCell::new(Some(lock)),
                flags,
            },
            added_names: BTreeSet::new(),
            committed: false,
        }
    }
}

impl<'a> OpenShared<'a> {
    fn with_manager<T>(&self, f: impl FnOnce(&Manager) -> T) -> T {
        let lock = self.lock.borrow();
        let manager = lock.as_ref().expect("Lock must be held");
        f(manager)
    }

    fn with_manager_mut<T>(&self, f: impl FnOnce(&mut Manager) -> T) -> T {
        let mut lock = self.lock.borrow_mut();
        let manager = lock.as_mut().expect("Lock must be held");
        f(manager)
    }

    fn take_lock(&self) -> Option<RwLockWriteGuard<'a, Manager>> {
        self.lock.borrow_mut().take()
    }

    fn replace_lock(&self, lock: RwLockWriteGuard<'a, Manager>) {
        *self.lock.borrow_mut() = Some(lock);
    }

    fn wait_for_other_thread(&self) {
        drop(self.take_lock());
        core::hint::spin_loop();
        self.replace_lock(crate::lock_write!(MANAGER));
    }

    fn await_registered(
        &self,
        added_names: Option<&BTreeSet<String>>,
        mut lookup: impl for<'mgr> FnMut(&'mgr Manager) -> Option<LibraryLookup<'mgr>>,
    ) -> Option<LibraryLookup<'static>> {
        loop {
            let entry = self.with_manager(|manager| lookup(manager).map(LibraryLookup::into_owned));
            match entry {
                Some(lib)
                    if lib.is_relocated()
                        || added_names.is_some_and(|names| names.contains(lib.shortname())) =>
                {
                    return Some(lib);
                }
                Some(_) => self.wait_for_other_thread(),
                None => return None,
            }
        }
    }

    /// Pure name/alias lookup with spin-wait for concurrent loads.
    fn wait_for_library(
        &self,
        added_names: Option<&BTreeSet<String>>,
        shortname: &str,
    ) -> Option<LibraryLookup<'static>> {
        self.await_registered(added_names, |manager| manager.lookup(shortname))
    }

    /// Stat `path` once and look up by inode. On hit, records `shortname` as an alias.
    fn inode_fallback(
        &self,
        added_names: Option<&BTreeSet<String>>,
        path: &str,
        shortname: &str,
    ) -> Result<Option<LibraryLookup<'static>>> {
        let req_identity = crate::os::get_file_inode(path)?;
        let entry = self.await_registered(added_names, |manager| {
            manager.lookup_by_identity(&req_identity)
        });

        if let Some(lib) = entry.as_ref().filter(|lib| {
            lib.is_relocated() || added_names.is_some_and(|names| names.contains(lib.shortname()))
        }) {
            log::info!(
                "dlopen: Found existing library by inode match: requested [{}], existing [{}] (dev={}, ino={})",
                shortname,
                lib.name().unwrap_or(lib.shortname()),
                req_identity.dev,
                req_identity.ino
            );
            self.with_manager_mut(|manager| {
                manager.add_alias(lib.shortname(), shortname);
            });
        }

        Ok(entry)
    }

    fn prepare_relocation(&self, group_scope: &ModuleScope) -> ModuleScope {
        let group_scope = group_scope
            .iter()
            .filter_map(|module| module.as_loaded::<ExtraData>().cloned())
            .collect::<Vec<_>>();
        let relocation_scope =
            self.with_manager_mut(|manager| manager.relocation_scope(&group_scope, self.flags));
        drop(self.take_lock());
        ModuleScope::new(relocation_scope.iter())
    }
}

impl<'a> OpenContext<'a> {
    fn remove_added_libraries(&self, manager: &mut Manager) {
        for name in self.added_names.iter() {
            manager.remove(name);
        }
    }

    #[cfg(not(feature = "std"))]
    fn reserve_pending(&mut self, shortname: &str, full_name: &str) {
        let shortname = self.shared.with_manager_mut(|manager| {
            reserve_pending(
                shortname.to_owned(),
                full_name,
                None,
                self.shared.flags,
                manager,
            )
        });
        self.added_names.insert(shortname);
    }

    fn finish_existing(&mut self, path: &str, lib: LibraryLookup<'static>) -> ElfLibrary {
        let canonical_shortname = lib.into_shortname_owned();
        log::info!(
            "dlopen: Found existing library [{}] (canonical name: {})",
            path,
            canonical_shortname
        );
        let elf_lib = self.shared.with_manager_mut(|manager| {
            manager
                .open_existing(&canonical_shortname, self.shared.flags)
                .expect("Existing library must be retrievable")
        });
        self.committed = true;
        elf_lib
    }

    fn try_existing(&mut self, path: &str) -> Result<Option<ElfLibrary>> {
        let shortname = path.rsplit_once('/').map_or(path, |(_, name)| name);
        // Step 1: fast name/alias lookup — no stat.
        // Step 2: on miss, stat once and fall back to inode lookup.
        if let Some(lib) = self.shared.wait_for_library(None, shortname) {
            return Ok(Some(self.finish_existing(path, lib)));
        }

        match self.shared.inode_fallback(None, path, shortname) {
            Ok(Some(lib)) => Ok(Some(self.finish_existing(path, lib))),
            Ok(None) => Ok(None),
            Err(e) => {
                if path.contains('/') {
                    // full path lookups should report errors
                    Err(e)
                } else {
                    // short name lookups can fail without affecting correctness, so we treat it as a miss instead of an error
                    Ok(None)
                }
            }
        }
    }

    fn complete_relocation(
        &mut self,
        link_ctx: &LinkContext<String, ExtraData, GlobalMeta>,
        committed: impl IntoIterator<Item = KeyId>,
    ) {
        let mut lock = self
            .shared
            .take_lock()
            .unwrap_or_else(|| crate::lock_write!(MANAGER));
        lock.merge_link_context(link_ctx, committed, self.shared.flags);
        self.shared.replace_lock(lock);
    }

    fn library_scope(&self, root: &str) -> Arc<[LoadedDylib]> {
        self.shared.with_manager(|manager| {
            manager
                .library_scope(root)
                .expect("root library must have a dependency scope after linking")
        })
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
}

fn into_linker_error(err: crate::error::Error) -> elf_loader::Error {
    match err {
        crate::error::Error::LoaderError { err } => err,
        other => elf_loader::CustomError::Message(other.to_string().into()).into(),
    }
}

struct LinkResolver<'ctx, 'mgr, 'bytes> {
    shared: &'ctx OpenShared<'mgr>,
    added_names: &'ctx mut BTreeSet<String>,
    root_request: String,
    root_bytes: Option<&'bytes [u8]>,
}

struct DlopenVisible<'ctx, 'mgr> {
    shared: &'ctx OpenShared<'mgr>,
}

impl<'ctx, 'mgr> DlopenVisible<'ctx, 'mgr> {
    fn new(shared: &'ctx OpenShared<'mgr>) -> Self {
        Self { shared }
    }
}

impl VisibleModules<String, ExtraData> for DlopenVisible<'_, '_> {
    fn contains_key(&self, key: &String) -> bool {
        self.shared
            .with_manager(|manager| manager.visible_contains(key))
    }

    fn direct_deps(&self, key: &String) -> Option<Box<[String]>> {
        self.shared
            .with_manager(|manager| manager.visible_direct_deps(key))
    }

    fn module(&self, key: &String) -> Option<ModuleHandle> {
        self.shared
            .with_manager(|manager| manager.visible_loaded(key).map(Into::into))
    }
}

enum CandidateInput<'bytes> {
    Reader(Box<dyn ElfReader + 'bytes>),
    Script(Vec<String>),
}

impl<'ctx, 'mgr, 'bytes> LinkResolver<'ctx, 'mgr, 'bytes> {
    fn new(
        shared: &'ctx OpenShared<'mgr>,
        added_names: &'ctx mut BTreeSet<String>,
        root_request: &str,
        root_bytes: Option<&'bytes [u8]>,
    ) -> Self {
        Self {
            shared,
            added_names,
            root_request: root_request.to_owned(),
            root_bytes,
        }
    }

    fn reserve_pending(&mut self, shortname: &str, full_name: &str) {
        if self.added_names.contains(shortname) {
            return;
        }

        let identity = crate::os::get_file_inode(full_name).ok();
        let shortname = self.shared.with_manager_mut(|manager| {
            reserve_pending(
                shortname.to_owned(),
                full_name,
                identity,
                self.shared.flags,
                manager,
            )
        });
        self.added_names.insert(shortname);
    }

    fn resolve_found(
        &self,
        lib: LibraryLookup<'static>,
        visible: Option<&dyn Fn(&str) -> bool>,
    ) -> Option<ResolvedKey<'static, String>> {
        let shortname = lib.into_shortname_owned();
        if visible.map_or(true, |is_visible| is_visible(&shortname)) {
            Some(ResolvedKey::existing(shortname))
        } else {
            None
        }
    }

    fn resolve_existing_by_name(
        &self,
        shortname: &str,
        visible: Option<&dyn Fn(&str) -> bool>,
    ) -> Option<ResolvedKey<'static, String>> {
        if visible.is_some_and(|is_visible| is_visible(shortname)) {
            return Some(ResolvedKey::existing(shortname.to_owned()));
        }

        self.shared
            .wait_for_library(Some(&*self.added_names), shortname)
            .and_then(|lib| self.resolve_found(lib, visible))
    }

    fn resolve_existing_by_path(
        &self,
        path: &str,
        shortname: &str,
        visible: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Option<ResolvedKey<'static, String>>> {
        if visible.is_some_and(|is_visible| is_visible(shortname)) {
            return Ok(Some(ResolvedKey::existing(shortname.to_owned())));
        }

        Ok(self
            .shared
            .inode_fallback(Some(&*self.added_names), path, shortname)?
            .and_then(|lib| self.resolve_found(lib, visible)))
    }

    fn resolve_script(
        &mut self,
        visible: Option<&dyn Fn(&str) -> bool>,
        rpath: &[ElfPath],
        runpath: &[ElfPath],
        libs: Vec<String>,
    ) -> Result<ResolvedKey<'bytes, String>> {
        self.resolve_first(libs, |resolver, lib| {
            resolver.resolve_request(visible, rpath, runpath, &lib, None)
        })?
        .ok_or_else(|| find_lib_error("can not resolve linker script".to_string()))
    }

    fn resolve_candidate_path(
        &mut self,
        visible: Option<&dyn Fn(&str) -> bool>,
        rpath: &[ElfPath],
        runpath: &[ElfPath],
        path: &ElfPath,
        bytes: Option<&'bytes [u8]>,
    ) -> Result<ResolvedKey<'bytes, String>> {
        let shortname = path.file_name();
        if let Some(module) = self.resolve_existing_by_path(path.as_str(), shortname, visible)? {
            return Ok(module);
        }

        match self.load_candidate(path.as_str(), bytes)? {
            CandidateInput::Reader(reader) => {
                self.reserve_pending(shortname, path.as_str());
                Ok(ResolvedKey::load(shortname.to_owned(), reader))
            }
            CandidateInput::Script(libs) => self.resolve_script(visible, rpath, runpath, libs),
        }
    }

    fn load_candidate(
        &self,
        path: &str,
        bytes: Option<&'bytes [u8]>,
    ) -> Result<CandidateInput<'bytes>> {
        match bytes {
            Some(bytes) => self.load_candidate_bytes(path, bytes),
            None => self.load_candidate_file(path),
        }
    }

    fn load_candidate_bytes(
        &self,
        path: &str,
        bytes: &'bytes [u8],
    ) -> Result<CandidateInput<'bytes>> {
        if is_elf_input(bytes) {
            Ok(CandidateInput::Reader(Box::new(ElfBinary::new(
                path, bytes,
            ))))
        } else {
            Ok(CandidateInput::Script(get_linker_script_libs(bytes)))
        }
    }

    fn load_candidate_file(&self, path: &str) -> Result<CandidateInput<'bytes>> {
        let header = crate::os::read_file_limit(path, 64)?;
        if is_elf_input(&header) {
            Ok(CandidateInput::Reader(Box::new(ElfFile::from_path(path)?)))
        } else {
            let content = crate::os::read_file(path)?;
            Ok(CandidateInput::Script(get_linker_script_libs(&content)))
        }
    }

    fn resolve_first<Candidate>(
        &mut self,
        candidates: impl IntoIterator<Item = Candidate>,
        mut resolve: impl FnMut(&mut Self, Candidate) -> Result<ResolvedKey<'bytes, String>>,
    ) -> Result<Option<ResolvedKey<'bytes, String>>> {
        for candidate in candidates {
            match resolve(self, candidate) {
                Ok(module) => return Ok(Some(module)),
                Err(err) if should_continue_library_search(&err) => {}
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }

    fn resolve_search_paths(
        &mut self,
        visible: Option<&dyn Fn(&str) -> bool>,
        rpath: &[ElfPath],
        runpath: &[ElfPath],
        paths: impl IntoIterator<Item = ElfPath>,
        bytes: Option<&'bytes [u8]>,
    ) -> Result<Option<ResolvedKey<'bytes, String>>> {
        self.resolve_first(paths, |resolver, path| {
            resolver.resolve_candidate_path(visible, rpath, runpath, &path, bytes)
        })
    }

    fn resolve_request(
        &mut self,
        visible: Option<&dyn Fn(&str) -> bool>,
        rpath: &[ElfPath],
        runpath: &[ElfPath],
        lib_name: &str,
        bytes: Option<&'bytes [u8]>,
    ) -> Result<ResolvedKey<'bytes, String>> {
        let shortname = LoaderPath::new(lib_name).file_name();
        if let Some(module) = self.resolve_existing_by_name(shortname, visible) {
            return Ok(module);
        }

        if lib_name.contains('/') {
            let path = ElfPath::from(lib_name);
            return self.resolve_candidate_path(visible, rpath, runpath, &path, bytes);
        }

        let rpath_dirs = if runpath.is_empty() { rpath } else { &[] };
        let search_dirs = rpath_dirs
            .iter()
            .chain(LD_LIBRARY_PATH.iter())
            .chain(runpath.iter());
        if let Some(module) = self.resolve_search_paths(
            visible,
            rpath,
            runpath,
            search_dirs.map(|dir| dir.join(lib_name)),
            bytes,
        )? {
            return Ok(module);
        }

        if let Some(cached_path) = LD_CACHE
            .as_ref()
            .and_then(|cache| cache.lookup(lib_name))
            .map(ElfPath::from)
        {
            match self.resolve_candidate_path(visible, rpath, runpath, &cached_path, bytes) {
                Ok(module) => return Ok(module),
                Err(err) if should_continue_library_search(&err) => {}
                Err(err) => return Err(err),
            }
        }

        if let Some(module) = self.resolve_search_paths(
            visible,
            rpath,
            runpath,
            DEFAULT_PATH.iter().map(|dir| dir.join(lib_name)),
            bytes,
        )? {
            return Ok(module);
        }

        Err(find_lib_error(format!(
            "can not find library: {}",
            lib_name
        )))
    }
}

impl<'ctx, 'mgr, 'bytes> KeyResolver<'bytes, String> for LinkResolver<'ctx, 'mgr, 'bytes> {
    fn load_root(
        &mut self,
        req: &RootRequest<'_, String>,
    ) -> core::result::Result<ResolvedKey<'bytes, String>, elf_loader::Error> {
        let key = req.key();
        let bytes = if *key == self.root_request {
            self.root_bytes.take()
        } else {
            None
        };
        self.resolve_request(None, &[], &[], key, bytes)
            .map_err(into_linker_error)
    }

    fn resolve_dependency(
        &mut self,
        req: &DependencyRequest<'_, String>,
    ) -> core::result::Result<ResolvedKey<'bytes, String>, elf_loader::Error> {
        let owner_name = req.owner_name();
        let rpath = req
            .rpath()
            .map(|r| fixup_rpath(owner_name, r))
            .unwrap_or_default();
        let runpath = req
            .runpath()
            .map(|r| fixup_rpath(owner_name, r))
            .unwrap_or_default();
        let is_visible = |key: &str| req.is_visible(&key.to_owned());
        self.resolve_request(Some(&is_visible), &rpath, &runpath, req.needed(), None)
            .map_err(into_linker_error)
    }
}

struct DlopenPlanner<'ctx, 'mgr> {
    shared: &'ctx OpenShared<'mgr>,
    relocation_scope: Option<ModuleScope>,
}

impl<'ctx, 'mgr> DlopenPlanner<'ctx, 'mgr> {
    fn new(shared: &'ctx OpenShared<'mgr>) -> Self {
        Self {
            shared,
            relocation_scope: None,
        }
    }
}

impl RelocationPlanner<String, ExtraData> for DlopenPlanner<'_, '_> {
    fn plan(
        &mut self,
        req: &RelocationRequest<'_, String, ExtraData>,
    ) -> core::result::Result<RelocationInputs<ExtraData>, elf_loader::Error> {
        if self.relocation_scope.is_none() {
            self.relocation_scope = Some(self.shared.prepare_relocation(req.scope()));
        }

        log::debug!("Planning relocation for dylib [{}]", req.key());

        let relocation_scope = self
            .relocation_scope
            .as_ref()
            .expect("Relocation scope must be initialized");
        let inputs = RelocationInputs::scope(relocation_scope.clone());
        if self.shared.flags.is_now() {
            Ok(inputs.eager())
        } else if self.shared.flags.is_lazy() {
            Ok(inputs.lazy())
        } else {
            Ok(inputs)
        }
    }
}

fn link_root<'mgr, 'bytes>(
    mut ctx: OpenContext<'mgr>,
    root_request: &str,
    root: LinkRoot<'bytes>,
) -> Result<ElfLibrary> {
    #[cfg(not(feature = "std"))]
    if let LinkRoot::Mapped { key, raw } = &root {
        if let Some(lib) = ctx.shared.wait_for_library(None, key) {
            return Ok(ctx.finish_existing(raw.name(), lib));
        }
        ctx.reserve_pending(key, raw.name());
    }

    let key_resolver = LinkResolver::new(
        &ctx.shared,
        &mut ctx.added_names,
        root_request,
        root.bytes(),
    );
    let visible_modules = DlopenVisible::new(&ctx.shared);
    let mut link_ctx = LinkContext::new();
    let relocation_planner = DlopenPlanner::new(&ctx.shared);
    let mut linker = Linker::<String, ()>::new()
        .map_loader(|_| new_loader())
        .visible_modules(visible_modules)
        .resolver(key_resolver)
        .planner(relocation_planner);
    let load_result = match root {
        LinkRoot::Load { key, .. } => linker.load(&mut link_ctx, key)?,
        #[cfg(not(feature = "std"))]
        LinkRoot::Mapped { key, raw } => linker.load_mapped_root(&mut link_ctx, key, raw)?,
    };
    drop(linker);

    let root_shortname = load_result.root().shortname().to_owned();
    ctx.complete_relocation(&link_ctx, load_result.committed().iter().copied());

    drop(link_ctx);

    let deps = ctx.library_scope(&root_shortname);
    Ok(ctx.finish(deps))
}

fn dlopen_impl(path: &str, flags: OpenFlags, bytes: Option<&[u8]>) -> Result<ElfLibrary> {
    let mut ctx = OpenContext::new(flags);

    log::info!(
        "dlopen: Try to open [{}] with [{:?}] ",
        path,
        ctx.shared.flags
    );

    if let Some(lib) = ctx.try_existing(path)? {
        return Ok(lib);
    }

    if ctx.shared.flags.is_noload() {
        return Err(find_lib_error(format!("can not find file: {}", path)));
    }

    link_root(
        ctx,
        path,
        LinkRoot::Load {
            key: path.to_owned(),
            bytes,
        },
    )
}

#[cfg(not(feature = "std"))]
pub(crate) fn dlopen_mapped_root(
    root_request: &str,
    raw: ElfDylib,
    flags: OpenFlags,
) -> Result<ElfLibrary> {
    let root_key = shortname_from_name(raw.name()).to_owned();
    let ctx = OpenContext::new(flags);

    log::info!(
        "dlopen: Link mapped root [{}] as [{}] with [{:?}]",
        root_request,
        root_key,
        ctx.shared.flags
    );

    link_root(ctx, root_request, LinkRoot::Mapped { key: root_key, raw })
}

static LD_LIBRARY_PATH: Lazy<Box<[ElfPath]>> = Lazy::new(|| {
    if let Some(path) = get_env("LD_LIBRARY_PATH") {
        parse_path_list(path)
    } else {
        Box::new([])
    }
});
static DEFAULT_PATH: Lazy<Box<[ElfPath]>> = Lazy::new(|| {
    let mut v = Vec::new();
    push_platform_default_paths(&mut v);
    v.push(ElfPath::from("/lib"));
    v.push(ElfPath::from("/usr/lib"));
    v.push(ElfPath::from("/lib64"));
    v.push(ElfPath::from("/usr/lib64"));
    v.into_boxed_slice()
});
static LD_CACHE: Lazy<Option<LdCache>> = Lazy::new(|| LdCache::new().ok());

#[cfg(target_arch = "x86_64")]
fn push_platform_default_paths(paths: &mut Vec<ElfPath>) {
    paths.push(ElfPath::from("/lib/x86_64-linux-gnu"));
    paths.push(ElfPath::from("/usr/lib/x86_64-linux-gnu"));
}

#[cfg(target_arch = "aarch64")]
fn push_platform_default_paths(paths: &mut Vec<ElfPath>) {
    paths.push(ElfPath::from("/lib/aarch64-linux-gnu"));
    paths.push(ElfPath::from("/usr/lib/aarch64-linux-gnu"));
}

#[cfg(target_arch = "riscv64")]
fn push_platform_default_paths(paths: &mut Vec<ElfPath>) {
    paths.push(ElfPath::from("/lib/riscv64-linux-gnu"));
    paths.push(ElfPath::from("/usr/lib/riscv64-linux-gnu"));
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
fn push_platform_default_paths(_paths: &mut Vec<ElfPath>) {}

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
        .map(ElfPath::from)
        .collect()
}

fn should_continue_library_search(err: &crate::error::Error) -> bool {
    match err {
        #[cfg(feature = "std")]
        crate::error::Error::IO(err) => err.kind() == std::io::ErrorKind::NotFound,
        #[cfg(not(feature = "std"))]
        crate::error::Error::IO(msg) => {
            msg.contains("No such file")
                || msg.contains("ENOENT")
                || msg.contains("Failed to open file")
        }
        _ => false,
    }
}

#[inline]
fn is_elf_input(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
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
