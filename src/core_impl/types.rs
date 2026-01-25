use alloc::{boxed::Box, ffi::CString, string::String, vec::Vec};
use core::ffi::{c_char, c_void};
use elf_loader::elf::ElfDyn;

pub(crate) static mut ARGC: usize = 0;
pub(crate) static mut ARGV: *const *mut c_char = core::ptr::null();
pub(crate) static mut ENVP: *const *const c_char = core::ptr::null();

/// User data associated with a dynamic library, used for internal tracking and debugging information.
#[derive(Default)]
pub(crate) struct UserData {
    /// Canonical name of the library as a C-compatible string.
    pub(crate) c_name: Option<CString>,
    /// The link map entry for this library, following the glibc-compatible structure.
    pub(crate) link_map: Option<Box<LinkMap>>,
    /// List of libraries that this library depends on.
    pub(crate) needed_libs: Vec<String>,
    /// The ELF dynamic table.
    pub(crate) dynamic_table: Option<Box<[ElfDyn]>>,
}

impl core::fmt::Debug for UserData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut d = f.debug_struct("UserData");
        d.field("c_name", &self.c_name);
        d.field("link_map", &self.link_map);
        d.field("needed_libs", &self.needed_libs);
        d.field("dynamic_table", &self.dynamic_table);
        d.finish()
    }
}

impl UserData {
    #[inline]
    pub fn new() -> Self {
        Self {
            c_name: None,
            link_map: None,
            needed_libs: Vec::new(),
            dynamic_table: None,
        }
    }
}

/// A structure representing a link map entry, matching the layout expected by many debuggers.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(crate) struct LinkMap {
    /// Base address of the library.
    pub l_addr: *mut c_void,
    /// Absolute path to the library.
    pub l_name: *const c_char,
    /// Pointer to the ELF dynamic section.
    pub l_ld: *mut ElfDyn,
    /// Next entry in the link map.
    pub l_next: *mut LinkMap,
    /// Previous entry in the link map.
    pub l_prev: *mut LinkMap,
}

unsafe impl Send for LinkMap {}
unsafe impl Sync for LinkMap {}
