use dlopen_rs::rtld::RtldTlsBackend;

const TLS_TCB_SIZE: usize = 4096;
const TLS_TCB_ALIGN: usize = 64;

pub(crate) const fn backend() -> RtldTlsBackend {
    RtldTlsBackend {
        init_tcb,
        install_thread_pointer,
        tcb_size: TLS_TCB_SIZE,
        tcb_align: TLS_TCB_ALIGN,
    }
}

unsafe extern "C" fn init_tcb(tp: *mut u8) -> bool {
    if tp.is_null() {
        return false;
    }
    unsafe {
        crate::glibc::init_tcb(tp);
    }
    true
}

unsafe extern "C" fn install_thread_pointer(tp: *mut u8) -> bool {
    if tp.is_null() {
        return false;
    }

    crate::arch::install_thread_pointer(tp)
}
