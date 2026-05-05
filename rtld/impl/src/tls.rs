use dlopen_rs::rtld::RtldTlsBackend;

pub(crate) const fn backend() -> RtldTlsBackend {
    RtldTlsBackend {
        init_thread_pointer: install_initial_thread_pointer,
    }
}

unsafe extern "C" fn install_initial_thread_pointer(tp: *mut u8) -> bool {
    if tp.is_null() {
        return false;
    }

    unsafe {
        crate::glibc::init_initial_thread_control_block(tp);
    }

    crate::arch::install_thread_pointer(tp)
}
