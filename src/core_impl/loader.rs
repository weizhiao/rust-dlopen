use crate::core_impl::types::{ARGC, ARGV, ENVP, ExtraData, LinkMap};
use crate::utils::debug::add_debug_link_map;
use crate::{OpenFlags, Result, error::find_symbol_error};
use alloc::{
    boxed::Box,
    ffi::CString,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    ffi::{c_char, c_int},
    fmt::Debug,
};
use elf_loader::input::ElfFile;
use elf_loader::loader::LifecycleContext;
use elf_loader::{
    Loader,
    elf::{ElfDyn, ElfPhdr, abi::PT_DYNAMIC},
    image::{ElfCoreRef, LoadedCore, RawDylib, Symbol},
    input::{ElfBinary, ElfReader},
};

pub(crate) type ElfDylib = RawDylib<ExtraData>;
pub(crate) type LoadedDylib = LoadedCore<ExtraData>;
pub(crate) type CoreComponentRef = ElfCoreRef<ExtraData>;

/// Searches for a symbol in a list of relocated libraries.
///
/// Iterates through the provided libraries in order and returns the first matching symbol.
#[inline]
pub(crate) fn find_symbol<'lib, T>(
    libs: &'lib [LoadedDylib],
    name: &str,
) -> Result<Symbol<'lib, T>> {
    log::info!("Get the symbol [{}] in [{}]", name, libs[0].name());
    libs.iter()
        .find_map(|lib| unsafe { lib.get::<T>(name) })
        .ok_or(find_symbol_error(format!("can not find symbol:{}", name)))
}

/// Creates a closure for lazy symbol resolution.
///
/// The closure captures weak references to the library's dependencies and uses them
/// to resolve symbols at runtime. It respects the `RTLD_DEEPBIND` flag to determine
/// the search order between the local dependency scope and the global scope.
#[inline]
pub(crate) fn create_lazy_scope(
    deps: &[LoadedDylib],
    flags: OpenFlags,
) -> Arc<dyn for<'a> Fn(&'a str) -> Option<*const ()> + Send + Sync + 'static> {
    let deps_weak: Vec<CoreComponentRef> = deps
        .iter()
        .map(|dep| unsafe { dep.core_ref().downgrade() })
        .collect();
    Arc::new(move |name: &str| {
        let deepbind = flags.contains(OpenFlags::RTLD_DEEPBIND);

        let local_find = || {
            deps_weak.iter().find_map(|dep| unsafe {
                let lib = LoadedDylib::from_core(dep.upgrade().unwrap());
                lib.get::<()>(name).map(|sym| {
                    log::trace!(
                        "Lazy Binding: find symbol [{}] from [{}] in local scope ",
                        name,
                        lib.name()
                    );
                    let val = sym.into_raw();
                    assert!(lib.base() != val as usize);
                    val
                })
            })
        };

        if deepbind {
            local_find().or_else(|| crate::core_impl::register::global_find(name))
        } else {
            crate::core_impl::register::global_find(name).or_else(local_find)
        }
    })
}

fn from_impl<'a, I>(object: I) -> Result<ElfDylib>
where
    I: ElfReader + elf_loader::input::IntoElfReader<'a>,
{
    let mut dylib = Loader::new()
        .with_default_tls_resolver()
        .with_context::<ExtraData>()
        .with_init(|ctx: &LifecycleContext| {
            let argc = unsafe { *core::ptr::addr_of!(ARGC) };
            let argv = unsafe { *core::ptr::addr_of!(ARGV) };
            let envp = unsafe { *core::ptr::addr_of!(ENVP) as *const *mut c_char };
            type InitFn = unsafe extern "C" fn(c_int, *const *mut c_char, *const *mut c_char);
            if let Some(init) = ctx.func() {
                let init: InitFn = unsafe { core::mem::transmute(init) };
                unsafe { init(argc as c_int, argv, envp) };
            }
            if let Some(init_array) = ctx.func_array() {
                for &f in init_array {
                    let f: InitFn = unsafe { core::mem::transmute(f) };
                    unsafe { f(argc as c_int, argv, envp) };
                }
            }
        })
        .load_dylib(object)?;
    let needed_libs = dylib
        .needed_libs()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();

    let name = dylib.name().to_string();
    let base = dylib.base();
    let dynamic_ptr = dylib
        .phdrs()
        .iter()
        .find(|p| p.p_type == PT_DYNAMIC)
        .map(|p| (base + p.p_vaddr as usize) as *mut ElfDyn)
        .unwrap_or(core::ptr::null_mut());

    let user_data = dylib.user_data_mut().unwrap();
    user_data.needed_libs = needed_libs;
    let c_name = CString::new(name).unwrap();

    let mut link_map = Box::new(LinkMap {
        l_addr: base as *mut _,
        l_name: c_name.as_ptr(),
        l_ld: dynamic_ptr as *mut _,
        l_next: core::ptr::null_mut(),
        l_prev: core::ptr::null_mut(),
    });

    unsafe { add_debug_link_map(link_map.as_mut()) };
    user_data.link_map = Some(link_map);
    user_data.c_name = Some(c_name);
    Ok(dylib)
}

/// Represents a successfully loaded and relocated dynamic library.
///
/// This is the primary interface for interacting with a loaded library,
/// providing methods to look up symbols and inspect metadata.
#[derive(Clone)]
pub struct ElfLibrary {
    pub(crate) inner: LoadedDylib,
    pub(crate) flags: OpenFlags,
    /// The flattened dependency scope (Searchlist) used by this library.
    pub(crate) deps: Option<Arc<[LoadedDylib]>>,
}

impl Debug for ElfLibrary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dylib")
            .field("inner", &self.inner)
            .field("flags", &self.flags)
            .finish()
    }
}

impl ElfLibrary {
    /// Find and load a elf dynamic library from path.
    #[inline]
    pub(crate) fn from_file(path: impl AsRef<str>) -> Result<ElfDylib> {
        let file = ElfFile::from_path(path.as_ref())?;
        from_impl(file)
    }

    /// Load a elf dynamic library from bytes.
    #[inline]
    pub(crate) fn from_binary(bytes: &[u8], path: impl AsRef<str>) -> Result<ElfDylib> {
        let file = ElfBinary::new(path.as_ref(), bytes);
        from_impl(file)
    }
}

pub trait DylibExt {
    fn needed_libs(&self) -> &[String];
    fn shortname(&self) -> &str;
}

impl DylibExt for LoadedDylib {
    #[inline]
    fn needed_libs(&self) -> &[String] {
        &self.user_data().needed_libs
    }

    #[inline]
    fn shortname(&self) -> &str {
        let name = self.name();
        if name.is_empty() {
            "main"
        } else {
            self.short_name()
        }
    }
}

impl ElfLibrary {
    /// Get the name of the dynamic library.
    #[inline]
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Get the C-style name of the dynamic library.
    #[inline]
    pub fn cname(&self) -> *const c_char {
        self.inner
            .user_data()
            .c_name
            .as_ref()
            .map(|n| n.as_ptr())
            .unwrap_or(core::ptr::null())
    }

    /// Get the short name of the dynamic library.
    #[inline]
    pub fn shortname(&self) -> &str {
        self.inner.shortname()
    }

    /// Get the flags of the dynamic library.
    #[inline]
    pub fn flags(&self) -> OpenFlags {
        self.flags
    }

    /// Get the base address of the dynamic library.
    #[inline]
    pub fn base(&self) -> usize {
        self.inner.base()
    }

    /// Gets the memory length of the elf object map.
    #[inline]
    pub fn mapped_len(&self) -> usize {
        self.inner.mapped_len()
    }

    /// Get the program headers of the dynamic library.
    #[inline]
    pub fn phdrs(&self) -> Option<&[ElfPhdr]> {
        self.inner.phdrs()
    }

    /// Get the needed libs' name of the elf object.
    #[inline]
    pub fn needed_libs(&self) -> &[String] {
        self.inner.needed_libs()
    }

    /// Get a pointer to a function or static variable by symbol name.
    ///
    /// The symbol is interpreted as-is; no mangling is done. This means that symbols like `x::y` are
    /// most likely invalid.
    ///
    /// # Safety
    /// Users of this API must specify the correct type of the function or variable loaded.
    ///
    /// # Examples
    /// ```no_run
    /// # use dlopen_rs::{Symbol, ElfLibrary ,OpenFlags};
    /// # let lib = ElfLibrary::dlopen("awesome.so", OpenFlags::RTLD_NOW).unwrap();
    /// unsafe {
    ///     let awesome_function: Symbol<unsafe extern fn(f64) -> f64> =
    ///         lib.get("awesome_function").unwrap();
    ///     awesome_function(0.42);
    /// }
    /// ```
    /// A static variable may also be loaded and inspected:
    /// ```no_run
    /// # use dlopen_rs::{Symbol, ElfLibrary ,OpenFlags};
    /// # let lib = ElfLibrary::dlopen("awesome.so", OpenFlags::RTLD_NOW).unwrap();
    /// unsafe {
    ///     let awesome_variable: Symbol<*mut f64> = lib.get("awesome_variable").unwrap();
    ///     **awesome_variable = 42.0;
    /// };
    /// ```
    #[inline]
    pub unsafe fn get<'lib, T>(&'lib self, name: &str) -> Result<Symbol<'lib, T>> {
        find_symbol(self.deps.as_ref().unwrap(), name)
    }

    /// Load a versioned symbol from the dynamic library.
    ///
    /// # Examples
    /// ```no_run
    /// # use dlopen_rs::{Symbol, ElfLibrary ,OpenFlags};
    /// # let lib = ElfLibrary::dlopen("awesome.so", OpenFlags::RTLD_NOW).unwrap();
    /// let symbol = unsafe { lib.get_version::<fn()>("function_name", "1.0").unwrap() };
    /// ```
    #[cfg(feature = "version")]
    #[inline]
    pub unsafe fn get_version<'lib, T>(
        &'lib self,
        name: &str,
        version: &str,
    ) -> Result<Symbol<'lib, T>> {
        unsafe {
            self.inner
                .get_version(name, version)
                .ok_or(find_symbol_error(format!("can not find symbol:{}", name)))
        }
    }
}
