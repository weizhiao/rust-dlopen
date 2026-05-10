#![allow(dead_code)]

pub mod auxv {
    pub const AT_NULL: usize = 0;
    pub const AT_PHDR: usize = 3;
    pub const AT_PHENT: usize = 4;
    pub const AT_PHNUM: usize = 5;
    pub const AT_PAGESZ: usize = 6;
    pub const AT_BASE: usize = 7;
    pub const AT_ENTRY: usize = 9;
    pub const AT_PLATFORM: usize = 15;
    pub const AT_HWCAP: usize = 16;
    pub const AT_CLKTCK: usize = 17;
    pub const AT_FPUCW: usize = 18;
    pub const AT_SECURE: usize = 23;
    pub const AT_RANDOM: usize = 25;
    pub const AT_HWCAP2: usize = 26;
    pub const AT_HWCAP3: usize = 29;
    pub const AT_HWCAP4: usize = 30;
    pub const AT_EXECFN: usize = 31;
    pub const AT_SYSINFO: usize = 32;
    pub const AT_SYSINFO_EHDR: usize = 33;
    pub const AT_MINSIGSTKSZ: usize = 51;
}

pub mod elf {
    #[allow(unused_imports)]
    pub use elf_loader::elf::{
        ElfDyn, ElfDynamicTag, ElfHeader, ElfPhdr, ElfProgramType, ElfRel, ElfRelType, ElfRela,
        ElfRelocationType,
    };
    #[allow(unused_imports)]
    pub use elf_loader::{arch::NativeArch, relocation::RelocationArch};
}

pub mod debug {
    use core::ffi::{c_char, c_int, c_void};

    use super::elf::ElfPhdr;

    pub const RT_CONSISTENT: c_int = 0;
    pub const RT_ADD: c_int = 1;
    pub const RT_DELETE: c_int = 2;

    #[derive(Debug, Clone, Copy)]
    #[repr(C)]
    pub struct LinkMap {
        pub l_addr: *mut c_void,
        pub l_name: *const c_char,
        pub l_ld: *mut c_void,
        pub l_next: *mut LinkMap,
        pub l_prev: *mut LinkMap,
        pub l_real: *mut LinkMap,
        pub l_ns: isize,
        pub l_libname: *mut c_void,
        pub l_info: [*mut c_void; 84],
        pub l_phdr: *const ElfPhdr,
        pub l_entry: usize,
        pub l_phnum: u16,
        pub l_ldnum: u16,
        pub _pad_after_ldnum: u32,
        pub _reserved_tail: [usize; 56],
    }

    impl LinkMap {
        pub const fn zero() -> Self {
            Self {
                l_addr: core::ptr::null_mut(),
                l_name: core::ptr::null(),
                l_ld: core::ptr::null_mut(),
                l_next: core::ptr::null_mut(),
                l_prev: core::ptr::null_mut(),
                l_real: core::ptr::null_mut(),
                l_ns: 0,
                l_libname: core::ptr::null_mut(),
                l_info: [core::ptr::null_mut(); 84],
                l_phdr: core::ptr::null(),
                l_entry: 0,
                l_phnum: 0,
                l_ldnum: 0,
                _pad_after_ldnum: 0,
                _reserved_tail: [0; 56],
            }
        }
    }

    const _: [(); 1208] = [(); core::mem::size_of::<LinkMap>()];

    unsafe impl Send for LinkMap {}
    unsafe impl Sync for LinkMap {}

    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct RDebug {
        pub version: c_int,
        pub map: *mut LinkMap,
        pub brk: Option<extern "C" fn()>,
        pub state: c_int,
        pub ldbase: *mut c_void,
    }

    impl RDebug {
        pub const fn zero() -> Self {
            Self {
                version: 0,
                map: core::ptr::null_mut(),
                brk: None,
                state: RT_CONSISTENT,
                ldbase: core::ptr::null_mut(),
            }
        }
    }

    unsafe impl Send for RDebug {}
    unsafe impl Sync for RDebug {}
}
