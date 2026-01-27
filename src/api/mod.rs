//! c interface

pub(crate) mod dl_iterate_phdr;
pub(crate) mod dladdr;
pub(crate) mod dlopen;
pub(crate) mod dlsym;

use crate::core_impl::register::MANAGER;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::{c_int, c_void};
use crate::core_impl::loader::{LoadedDylib, DylibExt};

pub use self::dl_iterate_phdr::dl_iterate_phdr;
pub use self::dladdr::dladdr;
pub use self::dlopen::dlopen;
pub use self::dlsym::dlsym;

/// # Safety
/// It is the same as `dlclose`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlclose(handle: *const c_void) -> c_int {
    if handle.is_null() {
        return 0;
    }
    let deps = unsafe { Box::from_raw(handle as *mut Arc<[LoadedDylib]>) };
    let Some(dylib) = crate::lock_read!(MANAGER)
        .all
        .get(deps[0].shortname())
        .map(|v| v.get_lib())
    else {
        return -1;
    };
    log::info!("dlclose: Closing [{}]", dylib.name());
    drop(deps);
    0
}
