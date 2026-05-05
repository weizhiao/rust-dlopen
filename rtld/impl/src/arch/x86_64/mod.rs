use core::ffi::c_int;

mod entry;

pub(crate) const RTLD_NAME: &[u8] = b"ld-linux-x86-64.so.2\0";

pub(crate) const DL_NNS: usize = 16;
pub(crate) const EXEC_PAGESIZE: usize = 4096;
pub(crate) const FPU_DEFAULT: u16 = 0x037f;
pub(crate) const PTHREAD_MUTEX_RECURSIVE_NP: c_int = 1;
pub(crate) const STDERR_FILENO: c_int = 2;

pub(crate) const X86_CPU_FEATURES_SIZE: usize = 520;
pub(crate) const X86_HWCAP_FLAGS: [[u8; 9]; 3] =
    [*b"sse2\0\0\0\0\0", *b"x86_64\0\0\0", *b"avx512_1\0"];
pub(crate) const X86_PLATFORMS: [[u8; 9]; 4] = [
    *b"i586\0\0\0\0\0",
    *b"i686\0\0\0\0\0",
    *b"haswell\0\0",
    *b"xeon_phi\0",
];

pub(crate) fn install_thread_pointer(tp: *mut u8) -> bool {
    const ARCH_SET_FS: usize = 0x1002;
    let res = unsafe { syscalls::raw_syscall!(syscalls::Sysno::arch_prctl, ARCH_SET_FS, tp) };
    res <= -4096isize as usize
}
