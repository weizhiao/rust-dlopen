use crate::utils::debug::GDBDebug;
use elf_loader::elf::abi::{DT_DEBUG, DT_NULL, PT_DYNAMIC, PT_PHDR};
use elf_loader::elf::{ElfDyn, ElfHeader, ElfPhdr};

cfg_if::cfg_if! {
    if #[cfg(feature = "use-syscall")] {
        mod linux;
        pub(crate) use linux::*;
    } else if #[cfg(unix)] {
        mod unix_libc;
        pub(crate) use unix_libc::*;
    } else {
        pub(crate) fn read_file(_path: &str) -> crate::Result<alloc::boxed::Box<[u8]>> {
            Err(crate::Error::Unsupported)
        }
        pub(crate) unsafe fn get_r_debug() -> *mut GDBDebug {
            core::ptr::null_mut()
        }
    }
}

pub(crate) unsafe fn find_r_debug(
    phdr_addr: usize,
    phnum: usize,
    interpreter_base: usize,
) -> *mut GDBDebug {
    if phdr_addr == 0 || phnum == 0 {
        return core::ptr::null_mut();
    }

    let phdrs = unsafe { core::slice::from_raw_parts(phdr_addr as *const ElfPhdr, phnum) };

    // 1. Find load bias and PT_DYNAMIC
    let mut load_bias = None;
    let mut dynamic_phdr = None;
    for phdr in phdrs {
        if phdr.p_type == PT_PHDR {
            load_bias = Some(phdr_addr.wrapping_sub(phdr.p_vaddr as usize));
        } else if phdr.p_type == PT_DYNAMIC {
            dynamic_phdr = Some(phdr);
        }
    }

    if load_bias.is_none() {
        for phdr in phdrs {
            if phdr.p_type == 1 && phdr.p_offset == 0 {
                let linked_phdr_addr = phdr.p_vaddr as usize + 64;
                load_bias = Some(phdr_addr.wrapping_sub(linked_phdr_addr));
                break;
            }
        }
    }

    // 2. Try to find DT_DEBUG in dynamic section
    if let (Some(bias), Some(phdr)) = (load_bias, dynamic_phdr) {
        let mut cur = (bias.wrapping_add(phdr.p_vaddr as usize)) as *const ElfDyn;
        while !cur.is_null() && unsafe { (*cur).d_tag } != DT_NULL as _ {
            if unsafe { (*cur).d_tag } == DT_DEBUG as _ {
                let ptr = unsafe { (*cur).d_un as *mut GDBDebug };
                if !ptr.is_null() && unsafe { (*ptr).version } != 0 {
                    return ptr;
                }
            }
            cur = unsafe { cur.add(1) };
        }
    }

    // 3. Special Fallback for musl: Try finding _r_debug via interpreter base
    if interpreter_base != 0 {
        let ehdr = unsafe { &*(interpreter_base as *const ElfHeader) };
        let phdrs = unsafe {
            core::slice::from_raw_parts(
                (interpreter_base + ehdr.e_phoff as usize) as *const ElfPhdr,
                ehdr.e_phnum as usize,
            )
        };
        if let Some(phdr) = phdrs.iter().find(|p| p.p_type == PT_DYNAMIC) {
            let mut cur = (interpreter_base.wrapping_add(phdr.p_vaddr as usize)) as *const ElfDyn;
            while !cur.is_null() && unsafe { (*cur).d_tag } != DT_NULL as _ {
                if unsafe { (*cur).d_tag } == DT_DEBUG as _ {
                    let ptr = unsafe { (*cur).d_un as *mut GDBDebug };
                    if !ptr.is_null() && unsafe { (*ptr).version } != 0 {
                        return ptr;
                    }
                }
                cur = unsafe { cur.add(1) };
            }
        }
    }

    core::ptr::null_mut()
}
