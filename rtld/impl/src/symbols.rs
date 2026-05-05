use core::{
    ffi::{c_char, c_int, c_void},
    ptr::null_mut,
};

use crate::{
    globals::rtld_x86_cpu_features,
    runtime::{RTLD_FATAL_EXIT_STATUS, exit},
};

#[unsafe(no_mangle)]
pub extern "C" fn __rtld_version_placeholder() {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_debug_state() {}

#[unsafe(no_mangle)]
pub extern "C" fn __tls_get_addr(index: *const usize) -> *mut c_void {
    dlopen_rs::rtld::tls_get_addr(index)
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_find_dso_for_object(addr: *const c_void) -> *mut c_void {
    dlopen_rs::api::dl_find_dso_for_object(addr)
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
    let (tls_size, tls_align) = dlopen_rs::rtld::tls_static_info();
    unsafe {
        if !size.is_null() {
            size.write(tls_size);
        }
        if !align.is_null() {
            align.write(tls_align);
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
pub extern "C" fn __tunable_get_val(_id: usize, _value: *mut c_void, _callback: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_rtld_di_serinfo() -> c_int {
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_signal_error(
    _errcode: c_int,
    _objname: *const c_char,
    _occasion: *const c_char,
    _errstring: *const c_char,
) -> ! {
    exit(RTLD_FATAL_EXIT_STATUS)
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_signal_exception(
    _errcode: c_int,
    _exception: *const DlException,
    _occasion: *const c_char,
) -> ! {
    exit(RTLD_FATAL_EXIT_STATUS)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _dl_catch_exception(
    _exception: *mut DlException,
    operate: Option<unsafe extern "C" fn(*mut c_void)>,
    args: *mut c_void,
) -> c_int {
    if let Some(operate) = operate {
        unsafe { operate(args) };
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_exception_free(exception: *mut DlException) {
    if !exception.is_null() {
        unsafe {
            (*exception).objname = core::ptr::null();
            (*exception).errstring = core::ptr::null();
            (*exception).message_buffer = null_mut();
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_exception_create(
    exception: *mut DlException,
    objname: *const c_char,
    errstring: *const c_char,
) {
    if !exception.is_null() {
        unsafe {
            (*exception).objname = objname;
            (*exception).errstring = errstring;
            (*exception).message_buffer = null_mut();
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_exception_create_format(
    exception: *mut DlException,
    objname: *const c_char,
    fmt: *const c_char,
) {
    _dl_exception_create(exception, objname, fmt);
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_fatal_printf() -> ! {
    exit(RTLD_FATAL_EXIT_STATUS)
}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_x86_get_cpu_features() -> *const c_void {
    unsafe { rtld_x86_cpu_features() }
}

#[repr(C)]
pub struct DlException {
    objname: *const c_char,
    errstring: *const c_char,
    message_buffer: *mut c_void,
}
