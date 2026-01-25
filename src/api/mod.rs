//! c interface

pub(crate) mod dl_iterate_phdr;
pub(crate) mod dladdr;
pub(crate) mod dlopen;
pub(crate) mod dlsym;

use crate::core_impl::register::MANAGER;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::{c_int, c_void};
use crate::core_impl::loader::{RelocatedDylib, DylibExt};

pub use self::dl_iterate_phdr::dl_iterate_phdr;
pub use self::dladdr::dladdr;
pub use self::dlopen::dlopen;
pub use self::dlsym::dlsym;

/// # Safety
/// It is the same as `dlclose`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlclose(handle: *const c_void) -> c_int {
    let deps = unsafe { Box::from_raw(handle as *mut Arc<[RelocatedDylib]>) };
    let dylib = crate::lock_read!(MANAGER)
        .all
        .get(deps[0].shortname())
        .unwrap()
        .get_dylib();
    drop(deps);
    log::info!("dlclose: Closing [{}]", dylib.name());
    0
}
