use crate::{
    ElfLibrary, LinkMap,
    loader::{EH_FRAME_ID, EhFrame},
    register::MANAGER,
};
use core::{
    ffi::{c_int, c_void},
    ptr::null,
};

#[repr(C)]
pub struct DlFindObject {
    dlfo_flags: u64,
    dlfo_map_start: *mut c_void,
    dlfo_map_end: *mut c_void,
    dlfo_link_map: *mut c_void,
    dlfo_eh_frame: *mut c_void,
    __dflo_reserved: [u64; 7],
}

pub(crate) fn addr2dso(addr: usize) -> Option<ElfLibrary> {
    log::trace!("addr2dso: addr [{:#x}]", addr);
    MANAGER.read().all.values().find_map(|v| {
        let start = v.relocated_dylib_ref().base();
        let end = start + v.relocated_dylib_ref().map_len();
        log::trace!("addr2dso: [{}] [{:#x}]-[{:#x}]", v.shortname(), start, end);
        if (start..end).contains(&addr) {
            Some(v.get_dylib())
        } else {
            None
        }
    })
}

#[unsafe(no_mangle)]
extern "C" fn _dl_find_object(pc: *const c_void, dlfo: *mut DlFindObject) -> c_int {
    addr2dso(pc as usize)
        .map(|dylib| {
            let dlfo = unsafe { &mut *dlfo };
            dlfo.dlfo_flags = 0;
            dlfo.dlfo_map_start = dylib.base() as *mut c_void;
            dlfo.dlfo_map_end = (dylib.base() + dylib.map_len()) as *mut c_void;
            dlfo.dlfo_eh_frame = dylib
                .inner
                .user_data()
                .get(EH_FRAME_ID)
                .unwrap()
                .downcast_ref::<EhFrame>()
                .unwrap()
                .0 as *mut c_void;
            0
        })
        .unwrap_or(-1)
}

// 本函数的作用是解决__cxa_thread_atexit_impl导致的内存泄露问题
// 现在在程序退出时会调用__cxa_thread_atexit_impl注册的tls dtor
#[unsafe(no_mangle)]
extern "C" fn _dl_find_dso_for_object(_addr: usize) -> *const LinkMap {
    null()
}
