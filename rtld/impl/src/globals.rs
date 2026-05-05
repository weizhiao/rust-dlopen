use core::{
    ffi::{c_int, c_void},
    ptr::{addr_of_mut, null, null_mut},
};
use dlopen_rs::rtld_abi::debug::{LinkMap, RDebug};

pub(crate) use crate::arch::RTLD_NAME;
pub(crate) use crate::glibc::RtldGlobalRoAux;
use crate::glibc::{RtldGlobal, RtldGlobalRo};

pub(crate) const EMPTY_NAME: &[u8] = b"\0";

#[unsafe(no_mangle)]
pub static mut _dl_argv: *const *const u8 = null();

#[unsafe(no_mangle)]
pub static mut __libc_stack_end: *const usize = null();

#[unsafe(no_mangle)]
pub static mut _rtld_global: RtldGlobal = RtldGlobal::new();

#[unsafe(no_mangle)]
pub static mut _rtld_global_ro: RtldGlobalRo = RtldGlobalRo::new();

#[unsafe(no_mangle)]
pub static mut _r_debug: RDebug = RDebug::zero();

pub(crate) static mut MAIN_LINK_MAP: LinkMap = LinkMap::zero();
static mut INITIAL_SEARCHLIST: [*mut LinkMap; 2] = [null_mut(); 2];

#[unsafe(no_mangle)]
pub static mut __libc_enable_secure: c_int = 0;

#[unsafe(no_mangle)]
pub static mut __rseq_flags: u32 = 0;

#[unsafe(no_mangle)]
pub static mut __rseq_size: u32 = 0;

#[unsafe(no_mangle)]
pub static mut __rseq_offset: u64 = 0;

pub(crate) unsafe fn rtld_link_map() -> *mut LinkMap {
    unsafe { (&mut *addr_of_mut!(_rtld_global)).rtld_link_map() }
}

pub(crate) unsafe fn rtld_x86_cpu_features() -> *const c_void {
    unsafe { (&*addr_of_mut!(_rtld_global_ro)).x86_cpu_features() }
}

pub(crate) unsafe fn publish_rtld_globals(
    main: *mut LinkMap,
    rtld: *mut LinkMap,
    r_debug: RDebug,
    ro_aux: RtldGlobalRoAux,
) {
    unsafe {
        let global = &mut *addr_of_mut!(_rtld_global);
        let ro = &mut *addr_of_mut!(_rtld_global_ro);
        global.publish(main, ro.initial_searchlist(), r_debug);
        ro.publish(addr_of_mut!(INITIAL_SEARCHLIST), main, rtld, ro_aux);
    }
}
