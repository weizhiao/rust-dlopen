use crate::core_impl::{register::addr2dso, types::LinkMap};
use core::ffi::{c_int, c_void};
use elf_loader::elf::abi::PT_GNU_EH_FRAME;

#[repr(C)]
struct DlFindObject {
    dlfo_flags: usize,            // 0
    dlfo_map_start: *mut c_void,  // 8
    dlfo_map_end: *mut c_void,    // 16
    dlfo_link_map: *mut LinkMap,  // 24
    dlfo_eh_frame: *const c_void, // 32
    dlfo_reserved: [usize; 7],    // 40
}

#[unsafe(no_mangle)]
extern "C" fn _dl_find_object(pc: *const c_void, dlfo: *mut DlFindObject) -> c_int {
    let address = pc as usize;
    let dso = if let Some(dso) = addr2dso(address) {
        dso
    } else {
        return -1;
    };

    let user_data = dso.inner.user_data();
    let phdrs = dso.inner.phdrs().unwrap_or(&[]);

    let eh_frame = phdrs
        .iter()
        .find(|p| p.p_type == PT_GNU_EH_FRAME)
        .map(|p| dso.base() + p.p_vaddr as usize)
        .unwrap_or(0);

    let info = unsafe { &mut *dlfo };
    info.dlfo_flags = 0;
    info.dlfo_map_start = dso.base() as *mut c_void;
    info.dlfo_map_end = (dso.base() + dso.mapped_len()) as *mut c_void;
    info.dlfo_link_map = user_data
        .link_map
        .as_ref()
        .map(|lm| lm.as_ref() as *const _ as *mut _)
        .unwrap_or(core::ptr::null_mut());
    info.dlfo_eh_frame = eh_frame as *const c_void;
    for i in 0..7 {
        info.dlfo_reserved[i] = 0;
    }

    log::info!(
        "_dl_find_object: success for address {:#x}: map_start={:#x}, map_end={:#x}, eh_frame={:#x}, link_map={:#x}",
        address,
        info.dlfo_map_start as usize,
        info.dlfo_map_end as usize,
        info.dlfo_eh_frame as usize,
        info.dlfo_link_map as usize
    );

    0
}
