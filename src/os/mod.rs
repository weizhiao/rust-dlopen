use crate::utils::debug::GDBDebug;
use elf_loader::elf::abi::DT_NULL;
use elf_loader::elf::{ElfDyn, ElfDynamicTag, ElfHeader, ElfPhdr, ElfProgramType};

cfg_if::cfg_if! {
    if #[cfg(feature = "use-syscall")] {
        mod linux;
        pub(crate) use linux::*;
    } else if #[cfg(unix)] {
        mod unix;
        pub(crate) use unix::*;
    } else {
        use crate::core_impl::types::FileIdentity;

        pub(crate) fn read_file(_path: &str) -> crate::Result<alloc::boxed::Box<[u8]>> {
            Err(crate::Error::Unsupported)
        }
        pub(crate) fn read_file_limit(_path: &str, _limit: usize) -> crate::Result<alloc::boxed::Box<[u8]>> {
            Err(crate::Error::Unsupported)
        }
        pub(crate) unsafe fn get_r_debug() -> *mut GDBDebug {
            core::ptr::null_mut()
        }
        pub(crate) fn get_file_inode(_path: &str) -> crate::Result<FileIdentity> {
            Err(crate::Error::Unsupported)
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
        if phdr.program_type() == ElfProgramType::PHDR {
            load_bias = Some(phdr_addr.wrapping_sub(phdr.p_vaddr()));
        } else if phdr.program_type() == ElfProgramType::DYNAMIC {
            dynamic_phdr = Some(phdr);
        }
    }

    if load_bias.is_none() {
        for phdr in phdrs {
            if phdr.program_type() == ElfProgramType::LOAD && phdr.p_offset() == 0 {
                let linked_phdr_addr = phdr.p_vaddr() + 64;
                load_bias = Some(phdr_addr.wrapping_sub(linked_phdr_addr));
                break;
            }
        }
    }

    // 2. Try to find DT_DEBUG in dynamic section
    if let (Some(bias), Some(phdr)) = (load_bias, dynamic_phdr) {
        let ptr =
            unsafe { find_debug_in_dynamic((bias.wrapping_add(phdr.p_vaddr())) as *const ElfDyn) };
        if !ptr.is_null() {
            return ptr;
        }
    }

    // 3. Special Fallback for musl: Try finding _r_debug via interpreter base
    if interpreter_base != 0 {
        let ehdr = unsafe { &*(interpreter_base as *const ElfHeader) };
        let phdrs = unsafe {
            core::slice::from_raw_parts(
                (interpreter_base + ehdr.e_phoff()) as *const ElfPhdr,
                ehdr.e_phnum(),
            )
        };
        if let Some(phdr) = phdrs
            .iter()
            .find(|p| p.program_type() == ElfProgramType::DYNAMIC)
        {
            let ptr = unsafe {
                find_debug_in_dynamic(
                    (interpreter_base.wrapping_add(phdr.p_vaddr())) as *const ElfDyn,
                )
            };
            if !ptr.is_null() {
                return ptr;
            }
        }
    }

    core::ptr::null_mut()
}

unsafe fn find_debug_in_dynamic(mut dynamic: *const ElfDyn) -> *mut GDBDebug {
    while !dynamic.is_null() && unsafe { (*dynamic).tag().raw() } != DT_NULL {
        if unsafe { (*dynamic).tag() } == ElfDynamicTag::DEBUG {
            let ptr = unsafe { (*dynamic).value() as *mut GDBDebug };
            if !ptr.is_null() && unsafe { (*ptr).version } != 0 {
                return ptr;
            }
        }
        dynamic = unsafe { dynamic.add(1) };
    }
    core::ptr::null_mut()
}
