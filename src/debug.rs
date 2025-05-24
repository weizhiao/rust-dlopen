use alloc::boxed::Box;
use spin::Mutex;

use crate::init::{GDBDebug, LinkMap};
use core::{
    ffi::{CStr, c_char, c_int},
    ptr::null_mut,
};
const RT_ADD: c_int = 1;
const RT_CONSISTENT: c_int = 0;
const RT_DELETE: c_int = 2;

pub(crate) struct CustomDebug {
    pub debug: *mut GDBDebug,
    pub tail: *mut LinkMap,
}

unsafe impl Sync for CustomDebug {}
unsafe impl Send for CustomDebug {}

pub(crate) struct DebugInfo {
    link_map: Box<LinkMap>,
}

impl Drop for DebugInfo {
    fn drop(&mut self) {
        unsafe {
            let mut custom_debug = DEBUG.lock();
            let tail = custom_debug.tail;
            let debug = &mut *custom_debug.debug;
            debug.state = RT_DELETE;
            (debug.brk)();
            match (
                debug.map == self.link_map.as_mut(),
                tail == self.link_map.as_mut(),
            ) {
                (true, true) => {
                    debug.map = null_mut();
                    custom_debug.tail = null_mut();
                }
                (true, false) => {
                    debug.map = self.link_map.l_next;
                    (*self.link_map.l_next).l_prev = null_mut();
                }
                (false, true) => {
                    let prev = &mut *self.link_map.l_prev;
                    prev.l_next = null_mut();
                    custom_debug.tail = prev;
                }
                (false, false) => {
                    let prev = &mut *self.link_map.l_prev;
                    let next = &mut *self.link_map.l_next;
                    prev.l_next = next;
                    next.l_prev = prev;
                }
            }
            debug.state = RT_CONSISTENT;
            (debug.brk)();
        }
    }
}

pub(crate) static DEBUG: Mutex<CustomDebug> = Mutex::new(CustomDebug {
    debug: null_mut(),
    tail: null_mut(),
});

impl DebugInfo {
    pub(crate) unsafe fn new(base: usize, name: *const c_char, dynamic: usize) -> DebugInfo {
        let mut custom_debug = DEBUG.lock();
        let tail = custom_debug.tail;
        assert!(
            !custom_debug.debug.is_null(),
            "Please call init function first"
        );
        let debug = unsafe { &mut *custom_debug.debug };
        let link_map = Box::leak(Box::new(LinkMap {
            l_addr: base as _,
            l_name: name as _,
            l_ld: dynamic as _,
            l_next: null_mut(),
            l_prev: tail,
        }));
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
        log::trace!("Add debugging information for [{}]", unsafe {
            CStr::from_ptr(name).to_str().unwrap()
        });
        DebugInfo {
            link_map: unsafe { Box::from_raw(link_map) },
        }
    }
}

#[inline]
pub(crate) fn init_debug(debug: &mut GDBDebug) {
    debug.map = null_mut();
    let mut custom = DEBUG.lock();
    custom.debug = debug;
    custom.tail = null_mut();
}
