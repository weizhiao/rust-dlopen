use core::{
    ffi::{c_int, c_void},
    ptr::null_mut,
};

use crate::{globals::rtld_x86_cpu_features, runtime::exit};

#[unsafe(no_mangle)]
pub extern "C" fn __rtld_version_placeholder() {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_debug_state() {}

#[unsafe(no_mangle)]
pub extern "C" fn __tls_get_addr(_index: *const usize) -> *mut c_void {
    null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_find_dso_for_object(_addr: *const c_void) -> *mut c_void {
    null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _dl_find_object(pc: *const c_void, dlfo: *mut c_void) -> c_int {
    unsafe { dlopen_rs::api::dl_find_object(pc, dlfo) }
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_allocate_tls(storage: *mut c_void) -> *mut c_void {
    storage
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_allocate_tls_init(storage: *mut c_void, _result: *mut c_void) -> *mut c_void {
    storage
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_deallocate_tls(_storage: *mut c_void, _dealloc_tcb: bool) {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_get_tls_static_info(size: *mut usize, align: *mut usize) {
    unsafe {
        if !size.is_null() {
            size.write(0);
        }
        if !align.is_null() {
            align.write(1);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn __nptl_change_stack_perm(_stack: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_audit_preinit(_link_map: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_audit_symbind_alt() -> *mut c_void {
    null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn __tunable_is_initialized() -> c_int {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn __tunable_get_val() {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_rtld_di_serinfo() -> c_int {
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_signal_error() -> ! {
    exit(127)
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_signal_exception() -> ! {
    exit(127)
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_catch_exception() -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_exception_free() {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_exception_create() {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_exception_create_format() {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_fatal_printf() -> ! {
    exit(127)
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_x86_get_cpu_features() -> *const c_void {
    unsafe { rtld_x86_cpu_features() }
}

#[cfg(not(feature = "hosted-check"))]
#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

#[cfg(not(feature = "hosted-check"))]
#[unsafe(no_mangle)]
pub extern "C" fn _Unwind_Resume() -> ! {
    exit(127)
}
