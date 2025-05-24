#[cfg(feature = "debug")]
use super::debug::DebugInfo;
use crate::{OpenFlags, Result, find_symbol_error};
use alloc::{boxed::Box, format, sync::Arc, vec::Vec};
use core::{any::Any, ffi::CStr, fmt::Debug};
use elf_loader::{
    CoreComponent, CoreComponentRef, ElfDylib, Loader, RelocatedDylib, Symbol, UserData,
    abi::PT_GNU_EH_FRAME,
    arch::{ElfPhdr, ElfRela},
    mmap::{Mmap, MmapImpl},
    object::{ElfBinary, ElfObject},
    segment::ElfSegments,
};

pub(crate) const EH_FRAME_ID: u8 = 0;
#[cfg(feature = "debug")]
pub(crate) const DEBUG_INFO_ID: u8 = 1;

pub(crate) struct EhFrame(pub usize);

impl EhFrame {
    pub(crate) fn new(eh_frame_hdr: usize) -> Self {
        EhFrame(eh_frame_hdr)
    }
}

#[inline]
pub(crate) fn find_symbol<'lib, T>(
    libs: &'lib [RelocatedDylib<'static>],
    name: &str,
) -> Result<Symbol<'lib, T>> {
    log::info!("Get the symbol [{}] in [{}]", name, libs[0].shortname());
    libs.iter()
        .find_map(|lib| unsafe { lib.get::<T>(name) })
        .ok_or(find_symbol_error(format!("can not find symbol:{}", name)))
}

pub trait Builder {
    fn create_object(path: &str) -> Result<impl ElfObject>;
}

#[inline(always)]
#[allow(unused)]
fn parse_phdr(
    cname: &CStr,
    phdr: &ElfPhdr,
    segments: &ElfSegments,
    data: &mut UserData,
) -> core::result::Result<(), Box<dyn Any + Send + Sync>> {
    match phdr.p_type {
        PT_GNU_EH_FRAME => {
            data.insert(
                EH_FRAME_ID,
                Box::new(EhFrame::new(phdr.p_vaddr as usize + segments.base())),
            );
        }
        #[cfg(feature = "debug")]
        elf_loader::abi::PT_DYNAMIC => {
            data.insert(
                DEBUG_INFO_ID,
                Box::new(unsafe {
                    DebugInfo::new(
                        segments.base(),
                        cname.as_ptr() as _,
                        segments.base() + phdr.p_vaddr as usize,
                    )
                }),
            );
        }
        #[cfg(feature = "tls")]
        elf_loader::abi::PT_TLS => {
            crate::tls::add_tls(segments, phdr, data, crate::tls::TlsState::Dynamic);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(feature = "tls")]
#[allow(unused)]
pub(crate) fn deal_unknown(
    rela: &ElfRela,
    lib: &CoreComponent,
    deps: &[&RelocatedDylib],
) -> core::result::Result<(), Box<dyn Any + Send + Sync>> {
    use crate::tls;

    fn find_tls_info(core: &elf_loader::CoreComponent) -> &crate::tls::TlsInfo {
        core.user_data()
            .get(crate::tls::TLS_INFO_ID)
            .unwrap()
            .downcast_ref::<crate::tls::TlsInfo>()
            .unwrap()
    }

    match rela.r_type() as _ {
        elf_loader::arch::REL_DTPMOD => {
            let r_sym = rela.r_symbol();
            let r_off = rela.r_offset();
            let ptr = (lib.base() + r_off) as *mut usize;
            if r_sym != 0 {
                let symdef = elf_loader::find_symdef(lib, deps, r_sym).unwrap();
                unsafe {
                    ptr.write(find_tls_info(symdef.lib).modid);
                }
                return Ok(());
            } else {
                unsafe { ptr.write(find_tls_info(lib).modid) };
                return Ok(());
            }
        }
        elf_loader::arch::REL_TPOFF => {
            let r_sym = rela.r_symbol();
            let r_off = rela.r_offset();
            let r_addend = rela.r_addend(lib.base());
            if r_sym != 0 {
                let symdef = elf_loader::find_symdef(lib, deps, r_sym).unwrap();
                let ptr = (lib.base() + r_off) as *mut usize;
                let tls_info = find_tls_info(symdef.lib);
                unsafe {
                    ptr.write(
                        symdef.sym.unwrap().st_value() + r_addend
                            - tls_info.static_tls_offset.unwrap(),
                    )
                };
				return Ok(());
            }
        }
        _ => {}
    }
    log::error!("Relocating dylib [{}] failed!", lib.name());
    Err(Box::new(()))
}

#[inline]
pub(crate) fn create_lazy_scope(
    deps: &[RelocatedDylib],
) -> Arc<dyn for<'a> Fn(&'a str) -> Option<*const ()>> {
    let deps_weak: Vec<CoreComponentRef> = deps
        .iter()
        .map(|dep| unsafe { dep.core_component_ref().downgrade() })
        .collect();
    Arc::new(move |name: &str| {
        deps_weak.iter().find_map(|dep| unsafe {
            let lib = RelocatedDylib::from_core_component(dep.upgrade().unwrap());
            lib.get::<()>(name).map(|sym| {
                log::trace!(
                    "Lazy Binding: find symbol [{}] from [{}] in local scope ",
                    name,
                    lib.name()
                );
                sym.into_raw()
            })
        })
    })
}

fn from_impl(object: impl ElfObject, flags: OpenFlags) -> Result<ElfDylib> {
    let mut loader = Loader::<MmapImpl>::new();
    loader.set_hook(Box::new(parse_phdr));
    #[cfg(feature = "fs")]
    unsafe {
        loader.set_init_params(
            crate::init::ARGC,
            (*core::ptr::addr_of!(crate::init::ARGV)).as_ptr() as usize,
            crate::init::ENVP,
        )
    };
    let lazy_bind = if flags.contains(OpenFlags::RTLD_LAZY) {
        Some(true)
    } else if flags.contains(OpenFlags::RTLD_NOW) {
        Some(false)
    } else {
        None
    };
    let dylib = loader.load_dylib(object, lazy_bind)?;
    log::debug!(
        "Loading dylib [{}] at address [0x{:x}-0x{:x}]",
        dylib.name(),
        dylib.base(),
        dylib.base() + dylib.map_len()
    );
    Ok(dylib)
}

pub(super) struct FileBuilder;

#[cfg(feature = "fs")]
impl Builder for FileBuilder {
    fn create_object(path: &str) -> Result<impl ElfObject> {
        use elf_loader::object::ElfFile;
        Ok(ElfFile::from_path(path)?)
    }
}

#[cfg(not(feature = "fs"))]
impl Builder for FileBuilder {
    fn create_object(path: &str) -> Result<impl ElfObject> {
        Err::<ElfBinary, crate::Error>(crate::find_lib_error(path))
    }
}

/// An relocated dynamic library
#[derive(Clone)]
pub struct ElfLibrary {
    pub(crate) inner: RelocatedDylib<'static>,
    pub(crate) flags: OpenFlags,
    pub(crate) deps: Option<Arc<Box<[RelocatedDylib<'static>]>>>,
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
    #[cfg(feature = "fs")]
    #[inline]
    pub(crate) fn from_file(path: impl AsRef<str>, flags: OpenFlags) -> Result<ElfDylib> {
        ElfLibrary::from_builder::<FileBuilder, MmapImpl>(path.as_ref(), flags)
    }

    #[inline]
    pub(crate) fn from_builder<B, M>(path: &str, flags: OpenFlags) -> Result<ElfDylib>
    where
        B: Builder,
        M: Mmap,
    {
        from_impl(B::create_object(path)?, flags)
    }

    /// Load a elf dynamic library from bytes.
    #[inline]
    pub(crate) fn from_binary(
        bytes: impl AsRef<[u8]>,
        path: impl AsRef<str>,
        flags: OpenFlags,
    ) -> Result<ElfDylib> {
        let file = ElfBinary::new(path.as_ref(), bytes.as_ref());
        from_impl(file, flags)
    }

    /// Get the name of the dynamic library.
    #[inline]
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Get the C-style name of the dynamic library.
    #[inline]
    pub fn cname(&self) -> &CStr {
        self.inner.cname()
    }

    /// Get the base address of the dynamic library.
    #[inline]
    pub fn base(&self) -> usize {
        self.inner.base()
    }

    /// Gets the memory length of the elf object map.
    #[inline]
    pub fn map_len(&self) -> usize {
        self.inner.map_len()
    }

    /// Get the program headers of the dynamic library.
    #[inline]
    pub fn phdrs(&self) -> &[ElfPhdr] {
        self.inner.phdrs()
    }

    /// Get the needed libs' name of the elf object.
    #[inline]
    pub fn needed_libs(&self) -> &[&str] {
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
