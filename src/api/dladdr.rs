use crate::{ElfLibrary, core_impl::register::addr2dso};
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    fmt::Debug,
    ptr::null,
};

#[repr(C)]
pub struct CDlinfo {
    pub dli_fname: *const c_char,
    pub dli_fbase: *mut c_void,
    pub dli_sname: *const c_char,
    pub dli_saddr: *mut c_void,
}

pub struct DlInfo {
    /// dylib
    dylib: ElfLibrary,
    /// Name of symbol whose definition overlaps addr
    sname: Option<&'static CStr>,
    /// Exact address of symbol named in dli_sname
    saddr: usize,
}

impl DlInfo {
    #[inline]
    pub fn dylib(&self) -> &ElfLibrary {
        &self.dylib
    }

    /// Name of symbol whose definition overlaps addr
    #[inline]
    pub fn symbol_name(&self) -> Option<&str> {
        self.sname.and_then(|s| s.to_str().ok())
    }

    /// Exact address of symbol
    #[inline]
    pub fn symbol_addr(&self) -> Option<usize> {
        if self.saddr == 0 {
            None
        } else {
            Some(self.saddr)
        }
    }
}

impl Debug for DlInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DlInfo")
            .field("dylib", &self.dylib)
            .field("sname", &self.sname)
            .field("saddr", &format_args!("{:#x}", self.saddr))
            .finish()
    }
}

impl ElfLibrary {
    /// determines whether the address specified in addr is located in one of the shared objects loaded by the calling
    /// application.  If it is, then `dladdr` returns information about the shared object and
    /// symbol that overlaps addr.
    pub fn dladdr(addr: usize) -> Option<DlInfo> {
        log::info!(
            "dladdr: Try to find the symbol information corresponding to [{:#x}]",
            addr
        );
        addr2dso(addr).map(|dylib| {
            let mut dl_info = DlInfo {
                dylib,
                sname: None,
                saddr: 0,
            };
            let symtab = dl_info.dylib.inner.symtab();
            let mut best_match: Option<(usize, &CStr)> = None;
            for i in 0..symtab.count_syms() {
                let (sym, syminfo) = symtab.symbol_idx(i);
                if sym.st_value() == 0 || !sym.is_ok_bind() || !sym.is_ok_type() {
                    continue;
                }
                let start = dl_info.dylib.base() + sym.st_value();
                let end = start + sym.st_size();
                if start <= addr && (sym.st_size() == 0 || addr < end) {
                    if let Some((best_start, _)) = best_match {
                        if start > best_start {
                            if let Some(cname) = syminfo.cname() {
                                best_match = Some((start, cname));
                            }
                        }
                    } else {
                        if let Some(cname) = syminfo.cname() {
                            best_match = Some((start, cname));
                        }
                    }
                }
            }
            if let Some((start, cname)) = best_match {
                dl_info.sname = Some(unsafe { core::mem::transmute(cname) });
                dl_info.saddr = start;
            }
            dl_info
        })
    }
}

/// # Safety
/// It is the same as `dladdr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dladdr(addr: *const c_void, info: *mut CDlinfo) -> c_int {
    if let Some(dl_info) = ElfLibrary::dladdr(addr as usize) {
        let info = unsafe { &mut *info };
        info.dli_fbase = dl_info.dylib().base() as _;
        info.dli_fname = dl_info.dylib().cname();
        info.dli_saddr = dl_info.symbol_addr().unwrap_or(0) as _;
        info.dli_sname = dl_info.sname.map_or(null(), |s| s.as_ptr());
        1
    } else {
        0
    }
}
