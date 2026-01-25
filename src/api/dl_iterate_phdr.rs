use crate::{ElfLibrary, Error, Result, core_impl::register::MANAGER};
use alloc::boxed::Box;
use core::{
    ffi::{c_char, c_int, c_ulonglong, c_void},
    ptr::null_mut,
};
use elf_loader::elf::ElfPhdr;

/// same as dl_phdr_info in libc
#[repr(C)]
pub struct CDlPhdrInfo {
    pub dlpi_addr: usize,
    pub dlpi_name: *const c_char,
    pub dlpi_phdr: *const ElfPhdr,
    pub dlpi_phnum: u16,
    pub dlpi_adds: c_ulonglong,
    pub dlpi_subs: c_ulonglong,
    pub dlpi_tls_modid: usize,
    pub dlpi_tls_data: *mut c_void,
}

pub struct DlPhdrInfo<'lib> {
    lib_base: usize,
    lib_name: *const c_char,
    phdrs: &'lib [ElfPhdr],
    dlpi_adds: c_ulonglong,
    dlpi_subs: c_ulonglong,
    tls_modid: usize,
    tls_data: Option<&'lib [u8]>,
}

impl DlPhdrInfo<'_> {
    /// Get the name of the dynamic library.
    #[inline]
    pub fn name(&self) -> &str {
        if self.lib_name.is_null() {
            ""
        } else {
            unsafe { core::ffi::CStr::from_ptr(self.lib_name).to_str().unwrap() }
        }
    }

    /// Get the C-style name of the dynamic library.
    #[inline]
    pub fn cname(&self) -> *const c_char {
        self.lib_name
    }

    /// Get the base address of the dynamic library.
    #[inline]
    pub fn base(&self) -> usize {
        self.lib_base
    }

    /// Get the program headers of the dynamic library.
    #[inline]
    pub fn phdrs(&self) -> &[ElfPhdr] {
        self.phdrs
    }
}

impl ElfLibrary {
    /// Iterate over the program headers of all dynamic libraries.
    pub fn dl_iterate_phdr<F>(mut callback: F) -> Result<()>
    where
        F: FnMut(&DlPhdrInfo) -> Result<()>,
    {
        let reader = crate::lock_read!(MANAGER);
        for lib in reader.all.values() {
            let user_data = lib.relocated_dylib_ref().user_data();
            let phdrs = lib.relocated_dylib_ref().phdrs().unwrap_or(&[]);
            if phdrs.is_empty() {
                continue;
            }
            let info = DlPhdrInfo {
                lib_base: lib.relocated_dylib_ref().base(),
                lib_name: user_data
                    .c_name
                    .as_ref()
                    .map(|n| n.as_ptr())
                    .unwrap_or(core::ptr::null()),
                phdrs,
                dlpi_adds: reader.all.len() as _,
                dlpi_subs: 0,
                tls_modid: 0,
                tls_data: None,
            };
            callback(&info)?;
        }
        Ok(())
    }
}

pub(crate) type CallBack =
    unsafe extern "C" fn(info: *mut CDlPhdrInfo, size: usize, data: *mut c_void) -> c_int;

/// # Safety
/// It is the same as `dl_iterate_phdr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_iterate_phdr(callback: Option<CallBack>, data: *mut c_void) -> c_int {
    let f = |info: &DlPhdrInfo| {
        if let Some(callback) = callback {
            let mut c_info = CDlPhdrInfo {
                dlpi_addr: info.lib_base,
                dlpi_name: info.lib_name,
                dlpi_phdr: info.phdrs.as_ptr(),
                dlpi_phnum: info.phdrs.len() as _,
                dlpi_adds: info.dlpi_adds,
                dlpi_subs: info.dlpi_subs,
                dlpi_tls_modid: info.tls_modid,
                dlpi_tls_data: info
                    .tls_data
                    .map(|data| data.as_ptr() as _)
                    .unwrap_or(null_mut()),
            };
            unsafe {
                let ret = callback(&mut c_info, size_of::<CDlPhdrInfo>(), data);
                if ret != 0 {
                    return Err(Error::IteratorPhdrError { err: Box::new(ret) });
                }
            };
        }
        Ok(())
    };
    if let Err(Error::IteratorPhdrError { err }) = ElfLibrary::dl_iterate_phdr(f) {
        *err.downcast::<i32>().unwrap()
    } else {
        0
    }
}
