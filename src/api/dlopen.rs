use crate::{
    OpenFlags, Result,
    core_impl::{
        loader::{DylibExt, ElfLibrary, LoadedDylib, new_loader},
        register::{DylibState, GlobalDylib, MANAGER, Manager, register},
        traits::AsFilename,
        types::ExtraData,
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
    vec,
    vec::Vec,
};
use core::{
    cell::RefCell,
    ffi::{CStr, c_char, c_int, c_void},
};
use elf_loader::input::{ElfBinary, ElfFile, ElfReader};
use elf_loader::linker::{
    DependencyRequest, KeyResolver, LinkContext, Linker, RelocationInputs, RelocationPlanner,
    RelocationRequest, ResolvedKey,
};
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
struct OpenContext<'a> {
    /// The write lock guard for the global library manager.
    /// Can be temporarily dropped to avoid deadlocks during relocation.
    lock: RefCell<Option<RwLockWriteGuard<'a, Manager>>>,
    /// Names of libraries that were added to the global registry in this operation.
    added_names: RefCell<BTreeSet<String>>,
    /// Loading flags for this operation.
    flags: OpenFlags,
    /// Indicates if the operation was successfully committed.
    committed: bool,
}

struct RelocationPlan {
    relocation_scope: Arc<[LoadedDylib]>,
}

impl<'a> Drop for OpenContext<'a> {
    fn drop(&mut self) {
        // If not committed, roll back changes to the global registry.
        if !self.committed {
            log::debug!("Destroying newly added dynamic libraries from the global");
            let mut lock = self
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
            lock: RefCell::new(Some(lock)),
            added_names: RefCell::new(BTreeSet::new()),
            flags,
            committed: false,
        }
    }

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

    fn has_added_name(&self, shortname: &str) -> bool {
        self.added_names.borrow().contains(shortname)
    }

    fn record_added_name(&self, shortname: String) {
        self.added_names.borrow_mut().insert(shortname);
    }

    fn with_added_libraries(&self, manager: &mut Manager, mut visit: impl FnMut(&mut GlobalDylib)) {
        let added_names = self.added_names.borrow();
        for name in added_names.iter() {
            let lib = manager.get_mut(name).expect("Library must be registered");
            visit(lib);
        }
    }

    fn remove_added_libraries(&self, manager: &mut Manager) {
        let added_names = self.added_names.borrow();
        for name in added_names.iter() {
            manager.remove(name);
        }
    }

    fn wait_for_other_thread(&self) {
        drop(self.take_lock());
        core::hint::spin_loop();
        self.replace_lock(crate::lock_write!(MANAGER));
    }

    fn await_registered(
        &self,
        mut lookup: impl FnMut(&Manager) -> Option<GlobalDylib>,
    ) -> Option<GlobalDylib> {
        loop {
            let entry = self.with_manager(|manager| lookup(manager));
            match entry {
                Some(lib) if lib.state.is_relocated() || self.has_added_name(lib.shortname()) => {
                    return Some(lib);
                }
                Some(_) => self.wait_for_other_thread(),
                None => return None,
            }
        }
    }

    fn finish_existing(&mut self, path: &str, lib: GlobalDylib) -> ElfLibrary {
        let canonical_shortname = lib.shortname().to_owned();
        log::info!(
            "dlopen: Found existing library [{}] (canonical name: {})",
            path,
            canonical_shortname
        );
        let elf_lib = self.with_manager_mut(|manager| {
            manager
                .open_existing(&canonical_shortname, self.flags)
                .expect("Existing library must be retrievable")
        });
        self.committed = true;
        elf_lib
    }

    fn prepare_relocation(&self, group_scope: &[LoadedDylib]) -> RelocationPlan {
        let group_keys = group_scope
            .iter()
            .map(|lib| lib.shortname().to_owned())
            .collect::<Vec<_>>();
        let relocation_scope = self.with_manager_mut(|manager| {
            self.register_pending_group(manager, &group_keys, group_scope);
            self.with_added_libraries(manager, |lib| {
                lib.state.set_relocating();
            });
            manager.relocation_scope(group_scope, self.flags)
        });
        drop(self.take_lock());
        RelocationPlan { relocation_scope }
    }

    fn try_existing(&mut self, path: &str) -> Result<Option<ElfLibrary>> {
        let shortname = path.rsplit_once('/').map_or(path, |(_, name)| name);
        // Step 1: fast name/alias lookup — no stat.
        // Step 2: on miss, stat once and fall back to inode lookup.
        if let Some(lib) = self.wait_for_library(shortname) {
            return Ok(Some(self.finish_existing(path, lib)));
        }

        match self.inode_fallback(path, shortname) {
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

    /// Pure name/alias lookup with spin-wait for concurrent loads.
    fn wait_for_library(&self, shortname: &str) -> Option<GlobalDylib> {
        self.await_registered(|manager| manager.get(shortname).cloned())
    }

    /// Stat `path` once and look up by inode. On hit, records `shortname` as an alias.
    fn inode_fallback(&self, path: &str, shortname: &str) -> Result<Option<GlobalDylib>> {
        let req_identity = crate::os::get_file_inode(path)?;
        let entry =
            self.await_registered(|manager| manager.get_by_identity(&req_identity).cloned());

        if let Some(lib) = entry.as_ref().filter(|lib| lib.state.is_relocated()) {
            log::info!(
                "dlopen: Found existing library by inode match: requested [{}], existing [{}] (dev={}, ino={})",
                shortname,
                lib.dylib_ref().name(),
                req_identity.dev,
                req_identity.ino
            );
            self.with_manager_mut(|manager| {
                manager.add_alias(lib.shortname(), shortname);
            });
        }

        Ok(entry)
    }

    fn resolve_found(
        &self,
        lib: GlobalDylib,
        visible: Option<&dyn Fn(&str) -> bool>,
    ) -> Option<ResolvedKey<'static, String>> {
        let shortname = lib.shortname().to_owned();
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

        self.wait_for_library(shortname)
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
            .inode_fallback(path, shortname)?
            .and_then(|lib| self.resolve_found(lib, visible)))
    }

    fn register_pending_group(
        &self,
        manager: &mut Manager,
        group_keys: &[String],
        group_scope: &[LoadedDylib],
    ) {
        for (key, lib) in group_keys.iter().zip(group_scope.iter()) {
            if manager.get(key).is_some() {
                continue;
            }

            let shortname = lib.shortname().to_owned();
            debug_assert_eq!(
                shortname, *key,
                "resolved load key must match the placeholder shortname"
            );
            register(lib.clone(), self.flags, manager, DylibState::default());
            self.record_added_name(shortname);
        }
    }

    fn complete_relocation(&self, link_ctx: &LinkContext<String, ExtraData>) {
        let mut lock = self
            .take_lock()
            .unwrap_or_else(|| crate::lock_write!(MANAGER));
        lock.merge_link_context(
            link_ctx,
            self.added_names
                .borrow()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.with_added_libraries(&mut lock, |lib| {
            lib.state.set_relocated();
        });
        self.replace_lock(lock);
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
    ctx: &'ctx OpenContext<'mgr>,
    root_request: String,
    root_bytes: Option<&'bytes [u8]>,
}

enum CandidateInput<'bytes> {
    Reader(Box<dyn ElfReader + 'bytes>),
    Script(Vec<String>),
}

impl<'ctx, 'mgr, 'bytes> LinkResolver<'ctx, 'mgr, 'bytes> {
    fn new(
        ctx: &'ctx OpenContext<'mgr>,
        root_request: &str,
        root_bytes: Option<&'bytes [u8]>,
    ) -> Self {
        Self {
            ctx,
            root_request: root_request.to_owned(),
            root_bytes,
        }
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
        let shortname = path
            .as_str()
            .rsplit_once('/')
            .map_or(path.as_str(), |(_, n)| n);
        if let Some(module) =
            self.ctx
                .resolve_existing_by_path(path.as_str(), shortname, visible)?
        {
            return Ok(module);
        }

        match self.load_candidate(path.as_str(), bytes)? {
            CandidateInput::Reader(reader) => Ok(ResolvedKey::load(shortname.to_owned(), reader)),
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
        let shortname = lib_name.rsplit_once('/').map_or(lib_name, |(_, name)| name);
        if let Some(module) = self.ctx.resolve_existing_by_name(shortname, visible) {
            return Ok(module);
        }

        if lib_name.contains('/') {
            let path = ElfPath::from_str(lib_name)?;
            return self.resolve_candidate_path(visible, rpath, runpath, &path, bytes);
        }

        let rpath_dirs = if runpath.is_empty() { rpath } else { &[] };
        if let Some(module) = self.resolve_search_paths(
            visible,
            rpath,
            runpath,
            rpath_dirs
                .iter()
                .chain(LD_LIBRARY_PATH.iter())
                .chain(runpath.iter())
                .map(|dir| dir.join(lib_name)),
            bytes,
        )? {
            return Ok(module);
        }

        let cached_path = LD_CACHE
            .as_ref()
            .and_then(|cache| cache.lookup(lib_name))
            .and_then(|path| ElfPath::from_str(&path).ok());
        if let Some(module) =
            self.resolve_search_paths(visible, rpath, runpath, cached_path, bytes)?
        {
            return Ok(module);
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

impl<'ctx, 'mgr, 'bytes> KeyResolver<'bytes, String, ExtraData>
    for LinkResolver<'ctx, 'mgr, 'bytes>
{
    fn load_root(
        &mut self,
        key: &String,
    ) -> core::result::Result<ResolvedKey<'bytes, String>, elf_loader::Error> {
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
        req: &DependencyRequest<'_, String, ExtraData>,
    ) -> core::result::Result<Option<ResolvedKey<'bytes, String>>, elf_loader::Error> {
        let rpath = req
            .rpath()
            .map(|r| fixup_rpath(req.owner().name(), r))
            .unwrap_or_default();
        let runpath = req
            .runpath()
            .map(|r| fixup_rpath(req.owner().name(), r))
            .unwrap_or_default();
        let is_visible = |key: &str| req.is_visible(&key.to_owned());
        self.resolve_request(Some(&is_visible), &rpath, &runpath, req.needed(), None)
            .map(Some)
            .map_err(into_linker_error)
    }
}

struct DlopenPlanner<'ctx, 'mgr> {
    ctx: &'ctx OpenContext<'mgr>,
    plan: Option<RelocationPlan>,
}

impl<'ctx, 'mgr> DlopenPlanner<'ctx, 'mgr> {
    fn new(ctx: &'ctx OpenContext<'mgr>) -> Self {
        Self { ctx, plan: None }
    }
}

impl RelocationPlanner<String, ExtraData> for DlopenPlanner<'_, '_> {
    fn plan(
        &mut self,
        req: &RelocationRequest<'_, String, ExtraData>,
    ) -> core::result::Result<RelocationInputs<ExtraData>, elf_loader::Error> {
        if self.plan.is_none() {
            self.plan = Some(self.ctx.prepare_relocation(req.scope()));
        }

        log::debug!("Planning relocation for dylib [{}]", req.key());

        let plan = self
            .plan
            .as_ref()
            .expect("Relocation plan must be initialized");
        let is_lazy = if self.ctx.flags.is_now() {
            false
        } else if self.ctx.flags.is_lazy() {
            true
        } else {
            req.raw().is_lazy()
        };
        let inputs = RelocationInputs::shared(plan.relocation_scope.clone());
        if is_lazy {
            Ok(inputs.lazy())
        } else {
            Ok(inputs.eager())
        }
    }
}

fn dlopen_impl(path: &str, flags: OpenFlags, bytes: Option<&[u8]>) -> Result<ElfLibrary> {
    let mut ctx = OpenContext::new(flags);

    log::info!("dlopen: Try to open [{}] with [{:?}] ", path, ctx.flags);

    if let Some(lib) = ctx.try_existing(path)? {
        return Ok(lib);
    }

    if ctx.flags.is_noload() {
        return Err(find_lib_error(format!("can not find file: {}", path)));
    }

    let key_resolver = LinkResolver::new(&ctx, path, bytes);
    let mut link_ctx = ctx.with_manager(|manager| manager.snapshot_link_context());
    let relocation_planner = DlopenPlanner::new(&ctx);
    let mut linker = Linker::<String, ExtraData>::new()
        .map_loader(|_| new_loader())
        .resolver(key_resolver)
        .planner(relocation_planner);
    let root = linker.load(&mut link_ctx, path.to_owned())?;

    ctx.complete_relocation(&link_ctx);

    let deps = link_ctx.dependency_scope(&root.shortname().to_owned());
    debug_assert!(
        !deps.is_empty(),
        "root library must have a dependency scope after linking"
    );
    drop(linker);
    drop(link_ctx);
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
