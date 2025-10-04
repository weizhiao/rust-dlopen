pub(crate) trait ModifyRegister {
    /// Get thread register base
    fn base() -> usize;
    unsafe fn get<T>(offset: usize) -> T;
    unsafe fn set(offset: usize, value: usize);
}

pub(crate) struct ThreadRegister;

#[cfg(target_arch = "x86_64")]
impl ModifyRegister for ThreadRegister {
    fn base() -> usize {
        let val;
        unsafe {
            core::arch::asm!("rdfsbase {}", out(reg) val);
        }
        val
    }

    unsafe fn get<T>(offset: usize) -> T {
        let size = core::mem::size_of::<T>();
        match size {
            2 => {
                let val: u16;
                unsafe {
                    core::arch::asm!("mov rax, {1}","mov {0:x}, fs:[rax]", out(reg) val, in(reg) offset);
                }
                unsafe { core::mem::transmute_copy(&val) }
            }
            4 => {
                let val: u32;
                unsafe {
                    core::arch::asm!("mov rax, {1}","mov {0:e}, fs:[rax]", out(reg) val, in(reg) offset);
                }
                unsafe { core::mem::transmute_copy(&val) }
            }
            8 => {
                let val: u64;
                unsafe {
                    core::arch::asm!("mov rax, {1}","mov {0}, fs:[rax]", out(reg) val, in(reg) offset);
                }
                unsafe { core::mem::transmute_copy(&val) }
            }
            _ => panic!("Unsupported type size"),
        }
    }

    unsafe fn set(offset: usize, value: usize) {
        unsafe {
            core::arch::asm!("mov rax, {1}","mov fs:[rax],{0}",in(reg) value, in(reg) offset);
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl ModifyRegister for ThreadRegister {
    fn get() -> usize {
        let val: usize;
        unsafe {
            core::arch::asm!("mrs {},tpidr_el0",out(reg) val);
        }
        val
    }

    unsafe fn set(value: usize) {
        unsafe {
            core::arch::asm!("msr tpidr_el0,{}",in(reg) value);
        }
    }
}

#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
impl ModifyRegister for ThreadRegister {
    fn get() -> usize {
        let val: usize;
        unsafe {
            core::arch::asm!("mv {}, tp",out(reg) val);
        }
        val
    }

    unsafe fn set(value: usize) {
        unsafe {
            core::arch::asm!("mv tp,{}",in(reg) value);
        }
    }
}
