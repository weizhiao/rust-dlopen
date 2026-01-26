use crate::error::parse_ld_cache_error;
use crate::{Error, Result};
use alloc::boxed::Box;
use alloc::vec::Vec;

impl From<syscalls::Errno> for Error {
    fn from(value: syscalls::Errno) -> Self {
        parse_ld_cache_error(value)
    }
}

pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
    // 确保字符串以 null 结尾，用于系统调用
    let mut path_c = Vec::from(path.as_bytes());
    path_c.push(0);

    const O_RDONLY: usize = 0;
    // 使用系统调用打开文件
    let fd = unsafe {
        #[cfg(any(
            target_arch = "aarch64",
            target_arch = "riscv64",
            target_arch = "riscv32"
        ))]
        {
            const AT_FDCWD: i32 = -100;
            syscalls::syscall4(
                syscalls::Sysno::openat,
                AT_FDCWD as usize,
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

    const SEEK_END: usize = 2;
    const SEEK_SET: usize = 0;
    // 获取文件大小
    let file_size =
        unsafe { syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_END)? };

    // 重置文件指针到开头
    unsafe { syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_SET)? };

    // 分配内存并读取文件内容
    let mut buffer = Vec::with_capacity(file_size);
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

    // 关闭文件
    unsafe { syscalls::syscall1(syscalls::Sysno::close, fd as usize)? };

    if bytes_read != file_size {
        return Err(parse_ld_cache_error("Failed to read complete file"));
    }

    Ok(buffer.into_boxed_slice())
}
