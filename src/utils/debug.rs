use crate::core_impl::types::{ExtraData, LinkMap};
use crate::rtld_abi::debug::{RT_ADD, RT_CONSISTENT, RT_DELETE};
use spin::Mutex;

use core::{
    ffi::CStr,
    ptr::{addr_of_mut, null_mut},
};

pub(crate) type GDBDebug = crate::rtld_abi::debug::RDebug;

pub(crate) struct CustomDebug {
    pub debug: *mut GDBDebug,
    pub tail: *mut LinkMap,
}

unsafe impl Sync for CustomDebug {}
unsafe impl Send for CustomDebug {}

extern "C" fn dlopen_debug_state() {}

static mut INTERNAL_GDB_DEBUG: GDBDebug = GDBDebug {
    version: 1,
    map: null_mut(),
    brk: Some(dlopen_debug_state),
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
    let owns_namespace = core::ptr::addr_eq(
        custom_debug.debug,
        core::ptr::addr_of_mut!(INTERNAL_GDB_DEBUG),
    );

    unsafe {
        if owns_namespace && (*link_map).l_real.is_null() {
            (*link_map).l_real = link_map;
        } else if !owns_namespace {
            (*link_map).l_real = null_mut();
        }
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
    unsafe { call_debug_state(debug) };
    debug.state = RT_CONSISTENT;
    unsafe { call_debug_state(debug) };
    log::trace!("Add debugging information for [{:?}]", unsafe {
        CStr::from_ptr((*link_map).l_name).to_string_lossy()
    });
}

impl Drop for ExtraData {
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
                call_debug_state(debug);
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
                call_debug_state(debug);
            }
        }
    }
}

unsafe fn call_debug_state(debug: &GDBDebug) {
    if let Some(brk) = debug.brk {
        brk();
    }
}
