use crate::core_impl::loader::find_symbol;
use crate::core_impl::register::{global_find, next_find};
use crate::{Result, Symbol, error::find_symbol_error};
use alloc::sync::Arc;
use core::{
    ffi::{CStr, c_char, c_void},
    ptr::null,
};

/// # Safety
/// It is the same as `dlsym`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *const c_void, symbol_name: *const c_char) -> *const c_void {
    const RTLD_DEFAULT: usize = 0;
    const RTLD_NEXT: usize = usize::MAX;
    let value = handle as usize;
    let name = match unsafe { CStr::from_ptr(symbol_name).to_str() } {
        Ok(name) => name,
        Err(_) => return null(),
    };

    let sym = if value == RTLD_DEFAULT {
        log::info!("dlsym: Use RTLD_DEFAULT flag to find symbol [{}]", name);
        dlsym_default::<()>(name).ok().map(|s| s.into_raw())
    } else if value == RTLD_NEXT {
        log::info!("dlsym: Use RTLD_NEXT flag to find symbol [{}]", name);
        unsafe { dlsym_next::<()>(name).ok().map(|s| s.into_raw()) }
    } else {
        let libs = unsafe { &*(handle as *const Arc<[crate::core_impl::loader::LoadedDylib]>) };
        let symbol = find_symbol::<()>(&libs[..], name)
            .ok()
            .map(|sym| sym.into_raw());
        symbol
    };
    sym.unwrap_or(null()).cast()
}

/// Find a symbol in the global search scope.
#[inline]
pub fn dlsym_default<T>(name: &str) -> Result<Symbol<'static, T>> {
    unsafe { global_find(name) }
        .ok_or_else(|| find_symbol_error(alloc::format!("can not find symbol:{}", name)))
}

/// Find the next occurrence of a symbol in the search order after the caller.
///
/// # Safety
/// This function uses inline assembly to determine the caller's address.
#[inline(always)]
pub unsafe fn dlsym_next<T>(name: &str) -> Result<Symbol<'static, T>> {
    let caller = unsafe {
        let ra: usize;
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!(
            "mov {}, [rbp + 8]",
            out(reg) ra,
            options(nostack, readonly)
        );
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!(
            "mov {}, lr",
            out(reg) ra,
            options(nostack, readonly)
        );
        #[cfg(target_arch = "riscv64")]
        core::arch::asm!(
            "mv {}, ra",
            out(reg) ra,
            options(nostack, readonly)
        );
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )))]
        let ra = 0;
        ra
    };
    unsafe { next_find(caller, name) }
        .ok_or_else(|| find_symbol_error(alloc::format!("can not find symbol:{}", name)))
}
