use std::{
    ffi::{c_char, c_int, c_void},
    ptr::null,
};

#[ctor::ctor]
fn init() {
    env_logger::init();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn dlinfo(_handle: *const c_void, _request: c_int, _info: *mut c_void) {
    todo!()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn dlerror() -> *const c_char {
    null()
}
