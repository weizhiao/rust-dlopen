//! c interface

mod dl_find_object;
pub(crate) mod dl_iterate_phdr;
pub(crate) mod dladdr;
pub(crate) mod dlopen;
pub mod dlsym;

use alloc::boxed::Box;
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
    let lib = unsafe { Box::from_raw(handle as *mut crate::ElfLibrary) };
    let shortname = lib.shortname();
    log::info!("dlclose: Closing [{}]", shortname);
    0
}
