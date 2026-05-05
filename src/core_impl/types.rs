use alloc::{boxed::Box, ffi::CString, string::String, vec::Vec};
use core::ffi::c_char;
use elf_loader::elf::ElfDyn;

pub(crate) type LinkMap = crate::rtld_abi::debug::LinkMap;

pub(crate) static mut ARGC: usize = 0;
pub(crate) static mut ARGV: *const *mut c_char = core::ptr::null();
pub(crate) static mut ENVP: *const *const c_char = core::ptr::null();

/// File identity information for detecting duplicate loads via different paths (e.g., symlinks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileIdentity {
    /// Device ID where the file resides.
    pub(crate) dev: u64,
    /// Inode number of the file.
    pub(crate) ino: u64,
}

/// User data associated with a dynamic library, used for internal tracking and debugging information.
#[derive(Default)]
pub(crate) struct ExtraData {
    /// Canonical name of the library as a C-compatible string.
    pub(crate) c_name: Option<CString>,
    /// The link map entry for this library, following the glibc-compatible structure.
    pub(crate) link_map: Option<Box<LinkMap>>,
    /// List of libraries that this library depends on.
    pub(crate) needed_libs: Vec<String>,
    /// The ELF dynamic table.
    pub(crate) dynamic_table: Option<Box<[ElfDyn]>>,
    /// File identity (device + inode) for detecting duplicate loads.
    pub(crate) file_identity: Option<FileIdentity>,
}

impl core::fmt::Debug for ExtraData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut d = f.debug_struct("UserData");
        d.field("c_name", &self.c_name);
        d.field("link_map", &self.link_map);
        d.field("needed_libs", &self.needed_libs);
        d.field("dynamic_table", &self.dynamic_table);
        d.field("file_identity", &self.file_identity);
        d.finish()
    }
}
