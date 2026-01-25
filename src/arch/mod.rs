#[cfg(target_arch = "x86_64")]
pub(crate) fn tp() -> usize {
    let val;
    unsafe {
        core::arch::asm!("rdfsbase {}", out(reg) val);
    }
    val
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn tp() -> usize {
    let val: usize;
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el0", out(reg) val);
    }
    val
}

#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
pub(crate) fn tp() -> usize {
    let val: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) val);
    }
    val
}
