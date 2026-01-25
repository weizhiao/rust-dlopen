use crate::core_impl::types::{LinkMap, UserData};
use spin::Mutex;

use core::{
    ffi::{CStr, c_int, c_void},
    ptr::{addr_of_mut, null_mut},
};

#[repr(C)]
pub(crate) struct GDBDebug {
    pub version: c_int,
    pub map: *mut LinkMap,
    pub brk: extern "C" fn(),
    pub state: c_int,
    pub ldbase: *mut c_void,
}

const RT_ADD: c_int = 1;
const RT_CONSISTENT: c_int = 0;
const RT_DELETE: c_int = 2;

pub(crate) struct CustomDebug {
    pub debug: *mut GDBDebug,
    pub tail: *mut LinkMap,
}

unsafe impl Sync for CustomDebug {}
unsafe impl Send for CustomDebug {}

#[unsafe(no_mangle)]
pub extern "C" fn _dl_debug_state() {}

static mut INTERNAL_GDB_DEBUG: GDBDebug = GDBDebug {
    version: 1,
    map: null_mut(),
    brk: _dl_debug_state,
    state: 0,
    ldbase: null_mut(),
};

pub(crate) static DEBUG: Mutex<CustomDebug> = Mutex::new(CustomDebug {
    debug: addr_of_mut!(INTERNAL_GDB_DEBUG),
    tail: null_mut(),
});

pub(crate) unsafe fn add_debug_link_map(link_map: *mut LinkMap) {
    let mut custom_debug = DEBUG.lock();
    let tail = custom_debug.tail;
    if custom_debug.debug.is_null() {
        return;
    }
    let debug = unsafe { &mut *custom_debug.debug };

    unsafe {
        (*link_map).l_prev = tail;
        (*link_map).l_next = null_mut();
    }

    if tail.is_null() {
        debug.map = link_map;
    } else {
        unsafe {
            (*tail).l_next = link_map;
        }
    }
    custom_debug.tail = link_map;
    debug.state = RT_ADD;
    (debug.brk)();
    debug.state = RT_CONSISTENT;
    (debug.brk)();
    log::trace!("Add debugging information for [{:?}]", unsafe {
        CStr::from_ptr((*link_map).l_name).to_string_lossy()
    });
}

impl Drop for UserData {
    fn drop(&mut self) {
        if let Some(link_map) = self.link_map.as_ref() {
            let link_map_ptr = core::ptr::addr_of!(**link_map) as *mut LinkMap;
            unsafe {
                let mut custom_debug = DEBUG.lock();
                if custom_debug.debug.is_null() {
                    return;
                }
                let tail = custom_debug.tail;
                let debug = &mut *custom_debug.debug;

                if debug.map != link_map_ptr && (*link_map_ptr).l_prev.is_null() {
                    return;
                }

                debug.state = RT_DELETE;
                (debug.brk)();
                match (debug.map == link_map_ptr, tail == link_map_ptr) {
                    (true, true) => {
                        debug.map = null_mut();
                        custom_debug.tail = null_mut();
                    }
                    (true, false) => {
                        debug.map = (*link_map_ptr).l_next;
                        (*(*link_map_ptr).l_next).l_prev = null_mut();
                    }
                    (false, true) => {
                        let prev = &mut *(*link_map_ptr).l_prev;
                        prev.l_next = null_mut();
                        custom_debug.tail = prev;
                    }
                    (false, false) => {
                        let prev = &mut *(*link_map_ptr).l_prev;
                        let next = &mut *(*link_map_ptr).l_next;
                        prev.l_next = next;
                        next.l_prev = prev;
                    }
                }
                debug.state = RT_CONSISTENT;
                (debug.brk)();
            }
        }
    }
}

#[inline]
pub(crate) fn init_debug(debug: &mut GDBDebug) {
    let mut custom = DEBUG.lock();
    custom.debug = debug;
    let mut cur = debug.map;
    if !cur.is_null() {
        unsafe {
            while !(*cur).l_next.is_null() {
                cur = (*cur).l_next;
            }
        }
    }
    custom.tail = cur;
}
