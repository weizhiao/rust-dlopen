use core::ffi::c_void;
use elf_loader::tls::{DefaultTlsResolver, TlsResolver};

#[cfg(all(feature = "use-syscall", target_arch = "x86_64"))]
use elf_loader::{
    Result, TlsError,
    tls::{TlsIndex, TlsInfo},
};

#[cfg(all(feature = "use-syscall", target_arch = "x86_64"))]
pub(crate) type ActiveTlsResolver = RtldTlsResolver;

#[cfg(not(all(feature = "use-syscall", target_arch = "x86_64")))]
pub(crate) type ActiveTlsResolver = DefaultTlsResolver;

pub(crate) extern "C" fn rtld_tls_get_addr(index: *const usize) -> *mut c_void {
    <ActiveTlsResolver as TlsResolver>::tls_get_addr(index.cast()).cast()
}

pub(crate) fn rtld_tls_static_info() -> (usize, usize) {
    #[cfg(all(feature = "use-syscall", target_arch = "x86_64"))]
    {
        rtld_static_tls::static_tls_info()
    }
    #[cfg(not(all(feature = "use-syscall", target_arch = "x86_64")))]
    {
        (0, 1)
    }
}

#[cfg(all(feature = "use-syscall", target_arch = "x86_64"))]
#[derive(Debug)]
pub(crate) struct RtldTlsResolver;

#[cfg(all(feature = "use-syscall", target_arch = "x86_64"))]
impl TlsResolver for RtldTlsResolver {
    fn register(tls_info: &TlsInfo) -> Result<usize> {
        <DefaultTlsResolver as TlsResolver>::register(tls_info)
    }

    fn register_static(tls_info: &TlsInfo) -> Result<(usize, isize)> {
        rtld_static_tls::register_static_module(tls_info)
    }

    fn add_static_tls(tls_info: &TlsInfo, offset: isize) -> Result<usize> {
        <DefaultTlsResolver as TlsResolver>::add_static_tls(tls_info, offset)
    }

    fn unregister(mod_id: usize) {
        <DefaultTlsResolver as TlsResolver>::unregister(mod_id);
    }

    extern "C" fn tls_get_addr(ti: *const TlsIndex) -> *mut u8 {
        <DefaultTlsResolver as TlsResolver>::tls_get_addr(ti)
    }
}

#[cfg(all(feature = "use-syscall", target_arch = "x86_64"))]
mod rtld_static_tls {
    use alloc::alloc::{alloc_zeroed, handle_alloc_error};
    use core::{alloc::Layout, ptr};
    use spin::Mutex;

    use super::*;

    const STATIC_TLS_ARENA_SIZE: usize = 1024 * 1024;
    const STATIC_TLS_TCB_SIZE: usize = 4096;

    struct StaticTlsArea {
        _base: *mut u8,
        tp: *mut u8,
        used: usize,
        max_align: usize,
    }

    unsafe impl Send for StaticTlsArea {}
    unsafe impl Sync for StaticTlsArea {}

    static STATIC_TLS: Mutex<Option<StaticTlsArea>> = Mutex::new(None);

    pub(super) fn register_static_module(tls_info: &TlsInfo) -> Result<(usize, isize)> {
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

        let offset = -(used as isize);
        let dst = unsafe { area.tp.offset(offset) };
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

    pub(super) fn static_tls_info() -> (usize, usize) {
        let static_tls = STATIC_TLS.lock();
        static_tls
            .as_ref()
            .map(|area| (area.used, area.max_align))
            .unwrap_or((0, 1))
    }

    fn ensure_static_tls_area() -> Result<StaticTlsArea> {
        let layout = Layout::from_size_align(STATIC_TLS_ARENA_SIZE + STATIC_TLS_TCB_SIZE, 4096)
            .map_err(|_| TlsError::StaticResolverUnsupported)?;
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            handle_alloc_error(layout);
        }

        let tp = unsafe { base.add(STATIC_TLS_ARENA_SIZE) };
        unsafe {
            ptr::write(tp.add(0x00) as *mut *mut u8, tp);
            ptr::write(tp.add(0x08) as *mut *mut u8, ptr::null_mut());
            ptr::write(tp.add(0x10) as *mut *mut u8, tp);
            ptr::write(tp.add(0x28) as *mut usize, 0x2f6a_5d1b_3c4e_8790);
            ptr::write(tp.add(0x30) as *mut usize, 0x6b43_1d29_84a0_7c5e);
        }
        install_initial_thread_pointer(tp)?;

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

    fn install_initial_thread_pointer(tp: *mut u8) -> Result<()> {
        const ARCH_SET_FS: usize = 0x1002;
        let res = unsafe { syscalls::raw_syscall!(syscalls::Sysno::arch_prctl, ARCH_SET_FS, tp) };
        if res > -4096isize as usize {
            return Err(TlsError::StaticResolverUnsupported.into());
        }
        Ok(())
    }
}
