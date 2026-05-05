pub use crate::abi::{auxv, debug, elf};

use crate::abi::{
    auxv::{AT_BASE, AT_ENTRY, AT_EXECFN, AT_NULL, AT_PHDR, AT_PHENT, AT_PHNUM},
    elf::ElfPhdr,
};
use crate::{
    OpenFlags, Result,
    api::dlopen::dlopen_mapped_root,
    core_impl::{
        ARGC, ARGV, ENVP, ElfDylib, LoadedDylib, MANAGER, RuntimeLoader, new_loader,
        register_loaded,
    },
    error::find_lib_error,
};
use alloc::string::String;
use core::ffi::{CStr, c_char};
use elf_loader::image::RawExec;

use self::bootstrap::{BootstrapMode, BootstrapObject, BootstrapState};

const RTLD_NAME: &str = "ld-linux-x86-64.so.2";

pub(crate) use self::tls::ActiveTlsResolver;
pub use self::tls::RtldTlsBackend;

/// Runs the replacement rtld stage-1 startup path.
///
/// # Safety
///
/// `state` must describe live mapped objects that remain mapped while stage-1
/// performs relocation and registration.
pub unsafe fn stage1(state: &BootstrapState) -> Result<usize> {
    if state.mode == BootstrapMode::DirectExec {
        return unsafe { prepare_direct_exec(state) };
    }

    unsafe { prepare_kernel_mapped_main(state) }
}

pub fn register_tls_backend(backend: RtldTlsBackend) {
    tls::register_backend(backend);
}

pub extern "C" fn tls_get_addr(index: *const usize) -> *mut core::ffi::c_void {
    tls::get_addr(index)
}

pub fn tls_static_info() -> (usize, usize) {
    tls::static_info()
}

mod tls {
    use alloc::alloc::{alloc_zeroed, handle_alloc_error};
    use core::{alloc::Layout, ffi::c_void, ptr};
    use elf_loader::{
        Result, TlsError,
        tls::{DefaultTlsResolver, TlsIndex, TlsInfo, TlsModuleId, TlsResolver, TlsTpOffset},
    };
    use spin::{Mutex, Once};

    const STATIC_TLS_ARENA_SIZE: usize = 1024 * 1024;
    const STATIC_TLS_TCB_SIZE: usize = 4096;

    pub(crate) type ActiveTlsResolver = RtldTlsResolver;

    pub(crate) extern "C" fn get_addr(index: *const usize) -> *mut c_void {
        <ActiveTlsResolver as TlsResolver>::tls_get_addr(index.cast()).cast()
    }

    pub(crate) fn static_info() -> (usize, usize) {
        let static_tls = STATIC_TLS.lock();
        static_tls
            .as_ref()
            .map(|area| (area.used, area.max_align))
            .unwrap_or((0, 1))
    }

    #[derive(Clone, Copy)]
    pub struct RtldTlsBackend {
        pub init_thread_pointer: unsafe extern "C" fn(*mut u8) -> bool,
    }

    static RTLD_TLS_BACKEND: Once<RtldTlsBackend> = Once::new();

    pub(crate) fn register_backend(backend: RtldTlsBackend) {
        RTLD_TLS_BACKEND.call_once(|| backend);
    }

    #[derive(Debug)]
    pub(crate) struct RtldTlsResolver;

    impl TlsResolver for RtldTlsResolver {
        fn register(tls_info: &TlsInfo) -> Result<TlsModuleId> {
            <DefaultTlsResolver as TlsResolver>::register(tls_info)
        }

        fn register_static(tls_info: &TlsInfo) -> Result<(TlsModuleId, TlsTpOffset)> {
            register_static_module(tls_info)
        }

        fn add_static_tls(tls_info: &TlsInfo, offset: TlsTpOffset) -> Result<TlsModuleId> {
            <DefaultTlsResolver as TlsResolver>::add_static_tls(tls_info, offset)
        }

        fn unregister(mod_id: TlsModuleId) {
            <DefaultTlsResolver as TlsResolver>::unregister(mod_id);
        }

        extern "C" fn tls_get_addr(ti: *const TlsIndex) -> *mut u8 {
            <DefaultTlsResolver as TlsResolver>::tls_get_addr(ti)
        }
    }

    struct StaticTlsArea {
        _base: *mut u8,
        tp: *mut u8,
        used: usize,
        max_align: usize,
    }

    unsafe impl Send for StaticTlsArea {}
    unsafe impl Sync for StaticTlsArea {}

    static STATIC_TLS: Mutex<Option<StaticTlsArea>> = Mutex::new(None);

    fn register_static_module(tls_info: &TlsInfo) -> Result<(TlsModuleId, TlsTpOffset)> {
        let mut static_tls = STATIC_TLS.lock();
        if static_tls.is_none() {
            *static_tls = Some(ensure_static_tls_area()?);
        }
        let area = static_tls
            .as_mut()
            .expect("rtld static TLS area should be initialized");
        let align = tls_info
            .align
            .max(1)
            .checked_next_power_of_two()
            .ok_or(TlsError::StaticResolverUnsupported)?;

        let used = align_up(
            area.used
                .checked_add(tls_info.memsz)
                .ok_or(TlsError::StaticResolverUnsupported)?,
            align,
        )
        .ok_or(TlsError::StaticResolverUnsupported)?;
        if used > STATIC_TLS_ARENA_SIZE {
            return Err(TlsError::StaticResolverUnsupported.into());
        }

        let offset = TlsTpOffset::new(-(used as isize));
        let dst = unsafe { area.tp.offset(offset.get()) };
        unsafe {
            ptr::copy_nonoverlapping(tls_info.image.as_ptr(), dst, tls_info.filesz);
            ptr::write_bytes(
                dst.add(tls_info.filesz),
                0,
                tls_info.memsz - tls_info.filesz,
            );
        }

        area.used = used;
        area.max_align = area.max_align.max(align);
        let id = <DefaultTlsResolver as TlsResolver>::add_static_tls(tls_info, offset)?;
        Ok((id, offset))
    }

    fn ensure_static_tls_area() -> Result<StaticTlsArea> {
        let layout = Layout::from_size_align(STATIC_TLS_ARENA_SIZE + STATIC_TLS_TCB_SIZE, 4096)
            .map_err(|_| TlsError::StaticResolverUnsupported)?;
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            handle_alloc_error(layout);
        }

        let tp = unsafe { base.add(STATIC_TLS_ARENA_SIZE) };
        init_thread_pointer(tp)?;

        Ok(StaticTlsArea {
            _base: base,
            tp,
            used: 0,
            max_align: 1,
        })
    }

    #[inline]
    fn align_up(value: usize, align: usize) -> Option<usize> {
        value
            .checked_add(align - 1)
            .map(|value| value & !(align - 1))
    }

    fn init_thread_pointer(tp: *mut u8) -> Result<()> {
        let Some(backend) = RTLD_TLS_BACKEND.get() else {
            return Err(TlsError::StaticResolverUnsupported.into());
        };
        if !unsafe { (backend.init_thread_pointer)(tp) } {
            return Err(TlsError::StaticResolverUnsupported.into());
        }
        Ok(())
    }
}

pub mod bootstrap {
    use crate::abi::elf::ElfPhdr;
    use core::ffi::c_void;

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum BootstrapMode {
        KernelMappedMain,
        DirectExec,
    }

    #[derive(Copy, Clone)]
    pub struct BootstrapObject {
        pub load_bias: usize,
        pub dynamic: *mut c_void,
        pub phdr: *const ElfPhdr,
        pub phnum: usize,
        pub entry: usize,
    }

    impl BootstrapObject {
        pub const fn zero() -> Self {
            Self {
                load_bias: 0,
                dynamic: core::ptr::null_mut(),
                phdr: core::ptr::null(),
                phnum: 0,
                entry: 0,
            }
        }
    }

    #[derive(Copy, Clone)]
    pub struct BootstrapState {
        pub argc: usize,
        pub argv: *const *const u8,
        pub envp: *const *const u8,
        pub auxv: *const usize,
        pub mode: BootstrapMode,
        pub exec_path: *const u8,
        pub main: BootstrapObject,
        pub rtld: BootstrapObject,
    }

    impl BootstrapState {
        pub const fn zero() -> Self {
            Self {
                argc: 0,
                argv: core::ptr::null(),
                envp: core::ptr::null(),
                auxv: core::ptr::null(),
                mode: BootstrapMode::KernelMappedMain,
                exec_path: core::ptr::null(),
                main: BootstrapObject::zero(),
                rtld: BootstrapObject::zero(),
            }
        }
    }
}

unsafe fn prepare_kernel_mapped_main(state: &BootstrapState) -> Result<usize> {
    unsafe {
        ARGC = state.argc;
        ARGV = state.argv as *const *mut c_char;
        ENVP = state.envp as *const *const c_char;
    }

    let mut loader = new_loader();
    let rtld = unsafe { load_borrowed(&mut loader, RTLD_NAME, state.rtld)? };
    let rtld = unsafe { LoadedDylib::from_core(rtld.core()) };
    register_loaded(
        rtld,
        OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NODELETE,
        &mut *crate::lock_write!(MANAGER),
    );

    let main = unsafe { load_borrowed(&mut loader, "", state.main)? };
    let entry = main.entry();
    let startup_flags = OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NOW | OpenFlags::RTLD_NODELETE;
    let root_request = if state.exec_path.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(state.exec_path.cast()) }
            .to_str()
            .unwrap_or("")
    };
    drop(dlopen_mapped_root(root_request, main, startup_flags)?);
    Ok(entry)
}

unsafe fn prepare_direct_exec(state: &BootstrapState) -> Result<usize> {
    unsafe {
        ARGC = state.argc;
        ARGV = state.argv as *const *mut c_char;
        ENVP = state.envp as *const *const c_char;
    }

    let exec_path = unsafe { CStr::from_ptr(state.exec_path.cast()) }
        .to_str()
        .map_err(|_| find_lib_error("direct exec path is not utf-8"))?;
    let mut loader = new_loader();
    let rtld = unsafe { load_borrowed(&mut loader, RTLD_NAME, state.rtld)? };
    let rtld = unsafe { LoadedDylib::from_core(rtld.core()) };
    register_loaded(
        rtld,
        OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NODELETE,
        &mut *crate::lock_write!(MANAGER),
    );

    let exec = loader.load_exec(exec_path)?;
    let (phdr, phnum) = exec
        .phdrs()
        .map(|phdrs| (phdrs.as_ptr() as usize, phdrs.len()))
        .unwrap_or((0, 0));
    let entry = exec.entry();
    unsafe {
        patch_exec_auxv(
            state.auxv as *mut usize,
            phdr,
            core::mem::size_of::<ElfPhdr>(),
            phnum,
            state.rtld.load_bias,
            entry,
            state.exec_path,
        );
    }

    match exec {
        RawExec::Dynamic(dynamic) => {
            let startup_flags =
                OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NOW | OpenFlags::RTLD_NODELETE;
            drop(dlopen_mapped_root(exec_path, dynamic, startup_flags)?);
            Ok(entry)
        }
        RawExec::Static(exec) => {
            core::mem::forget(exec);
            Ok(entry)
        }
    }
}

unsafe fn load_borrowed(
    loader: &mut RuntimeLoader,
    name: impl Into<String>,
    object: BootstrapObject,
) -> Result<ElfDylib> {
    if object.phdr.is_null() || object.phnum == 0 {
        return Err(find_lib_error(
            "bootstrap object is missing program headers",
        ));
    }

    let phdrs = unsafe { core::slice::from_raw_parts(object.phdr, object.phnum) }.to_vec();
    unsafe { loader.load_mapped_dynamic(name, object.load_bias, phdrs, object.entry) }
        .map_err(Into::into)
}

unsafe fn patch_exec_auxv(
    mut auxv: *mut usize,
    phdr: usize,
    phent: usize,
    phnum: usize,
    base: usize,
    entry: usize,
    exec_path: *const u8,
) {
    if auxv.is_null() {
        return;
    }

    loop {
        let kind = unsafe { auxv.read() };
        if kind == AT_NULL {
            return;
        }
        let value = unsafe { auxv.add(1) };
        match kind {
            AT_PHDR => unsafe { value.write(phdr) },
            AT_PHENT => unsafe { value.write(phent) },
            AT_PHNUM => unsafe { value.write(phnum) },
            AT_BASE => unsafe { value.write(base) },
            AT_ENTRY => unsafe { value.write(entry) },
            AT_EXECFN => unsafe { value.write(exec_path as usize) },
            _ => {}
        }
        auxv = unsafe { auxv.add(2) };
    }
}
