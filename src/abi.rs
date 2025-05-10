//! c interface

use crate::register::MANAGER;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::{c_int, c_void};
use elf_loader::RelocatedDylib;

pub use crate::dl_iterate_phdr::{CDlPhdrInfo, dl_iterate_phdr};
pub use crate::dladdr::{CDlinfo, dladdr};
pub use crate::dlopen::dlopen;
pub use crate::dlsym::dlsym;

/// # Safety
/// It is the same as `dlclose`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlclose(handle: *const c_void) -> c_int {
    let deps = unsafe { Arc::from_raw(handle as *const Box<[RelocatedDylib<'static>]>) };
    let dylib = MANAGER
        .read()
        .all
        .get(deps[0].shortname())
        .unwrap()
        .get_dylib();
    drop(deps);
    log::info!("dlclose: Closing [{}]", dylib.name());
    0
}

#[unsafe(no_mangle)]
// FIXME: 有内存泄漏
extern "C" fn __cxa_thread_atexit_impl() -> c_int {
    0
}
