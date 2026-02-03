//! c interface

mod dl_find_object;
pub(crate) mod dl_iterate_phdr;
pub(crate) mod dladdr;
pub(crate) mod dlopen;
pub mod dlsym;

use crate::core_impl::loader::{DylibExt, LoadedDylib};
use crate::core_impl::register::MANAGER;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::{c_int, c_void};

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
    let shortname = alloc::string::String::from(deps[0].shortname());
    log::info!("dlclose: Closing [{}]", shortname);
    drop(deps);

    let dylib = crate::lock_read!(MANAGER)
        .get(&shortname)
        .map(|v| v.get_lib());

    drop(dylib);
    // When dylib is dropped here, it will trigger ElfLibrary::drop
    // and attempt to destroy the library if its ref count reaches the threshold.
    0
}
