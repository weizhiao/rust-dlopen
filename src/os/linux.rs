use crate::error::parse_ld_cache_error;
use crate::utils::debug::GDBDebug;
use crate::{Error, Result};
use alloc::boxed::Box;
use alloc::vec::Vec;

impl From<syscalls::Errno> for Error {
    fn from(value: syscalls::Errno) -> Self {
        parse_ld_cache_error(value)
    }
}

const AT_PHDR: usize = 3;
const AT_PHNUM: usize = 5;
const AT_BASE: usize = 7;

pub(crate) unsafe fn get_r_debug() -> *mut GDBDebug {
    let phdr_addr = get_auxv(AT_PHDR);
    let phnum = get_auxv(AT_PHNUM);
    let base = get_auxv(AT_BASE);

    unsafe { crate::os::find_r_debug(phdr_addr, phnum, base) }
}

fn get_auxv(target_type: usize) -> usize {
    let Ok(data) = read_file("/proc/self/auxv") else {
        return 0;
    };
    let size = core::mem::size_of::<usize>();
    for chunk in data.chunks_exact(size * 2) {
        let type_ = usize::from_ne_bytes(chunk[..size].try_into().unwrap());
        let val = usize::from_ne_bytes(chunk[size..].try_into().unwrap());
        if type_ == target_type {
            return val;
        }
        if type_ == 0 {
            break;
        }
    }
    0
}

pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
    let mut path_c = Vec::from(path.as_bytes());
    path_c.push(0);

    const O_RDONLY: usize = 0;
    const SEEK_END: usize = 2;
    const SEEK_SET: usize = 0;

    let fd = unsafe {
        #[cfg(any(
            target_arch = "aarch64",
            target_arch = "riscv64",
            target_arch = "riscv32"
        ))]
        {
            syscalls::syscall4(
                syscalls::Sysno::openat,
                -100isize as usize,
                path_c.as_ptr() as usize,
                O_RDONLY,
                0,
            )?
        }
        #[cfg(target_arch = "x86_64")]
        {
            syscalls::syscall2(syscalls::Sysno::open, path_c.as_ptr() as usize, O_RDONLY)?
        }
    };

    let read_result = (|| -> Result<Box<[u8]>> {
        let mut buffer = Vec::new();
        let file_size = unsafe {
            syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_END).unwrap_or(0)
        };
        let _ = unsafe { syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_SET) };

        if file_size > 0 {
            buffer.reserve_exact(file_size);
            unsafe {
                buffer.set_len(file_size);
            }
            let bytes_read = unsafe {
                syscalls::syscall3(
                    syscalls::Sysno::read,
                    fd as usize,
                    buffer.as_mut_ptr() as usize,
                    file_size,
                )?
            };
            if bytes_read != file_size {
                return Err(parse_ld_cache_error("Failed to read complete file"));
            }
        } else {
            let mut temp = [0u8; 1024];
            loop {
                let bytes_read = unsafe {
                    syscalls::syscall3(
                        syscalls::Sysno::read,
                        fd as usize,
                        temp.as_mut_ptr() as usize,
                        temp.len(),
                    )?
                };
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp[..bytes_read]);
            }
        }
        Ok(buffer.into_boxed_slice())
    })();

    unsafe {
        let _ = syscalls::syscall1(syscalls::Sysno::close, fd as usize);
    }
    read_result
}
