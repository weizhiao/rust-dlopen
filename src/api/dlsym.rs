use crate::core_impl::loader::find_symbol;
use alloc::sync::Arc;
use core::{
    ffi::{CStr, c_char, c_void},
    ptr::null,
};

/// # Safety
/// It is the same as `dlsym`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *const c_void, symbol_name: *const c_char) -> *const c_void {
    const RTLD_DEFAULT: usize = 0;
    const RTLD_NEXT: usize = usize::MAX;
    let value = handle as usize;
    let name = unsafe { CStr::from_ptr(symbol_name).to_str().unwrap_unchecked() };
    let sym = if value == RTLD_DEFAULT {
        log::info!("dlsym: Use RTLD_DEFAULT flag to find symbol [{}]", name);
        crate::core_impl::register::global_find(name)
    } else if value == RTLD_NEXT {
        todo!("RTLD_NEXT is not supported")
    } else {
        let libs = unsafe { &*(handle as *const Arc<[crate::core_impl::loader::RelocatedDylib]>) };
        let symbol = find_symbol::<()>(&libs[..], name)
            .ok()
            .map(|sym| sym.into_raw());
        symbol
    };
    sym.unwrap_or(null()).cast()
}
