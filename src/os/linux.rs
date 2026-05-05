use crate::core_impl::FileIdentity;
use crate::{Error, Result};
use alloc::boxed::Box;
#[cfg(not(feature = "std"))]
use alloc::string::ToString;
use alloc::vec::Vec;

#[cfg(target_arch = "x86_64")]
#[repr(C)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_nlink: u64,
    st_mode: u32,
    st_uid: u32,
    st_gid: u32,
    __pad0: u32,
    st_rdev: u64,
    st_size: i64,
    st_blksize: i64,
    st_blocks: i64,
    st_atime: i64,
    st_atime_nsec: i64,
    st_mtime: i64,
    st_mtime_nsec: i64,
    st_ctime: i64,
    st_ctime_nsec: i64,
    __unused: [i64; 3],
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
#[repr(C)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad1: u64,
    st_size: i64,
    st_blksize: i32,
    __pad2: i32,
    st_blocks: i64,
    st_atime: i64,
    st_atime_nsec: u64,
    st_mtime: i64,
    st_mtime_nsec: u64,
    st_ctime: i64,
    st_ctime_nsec: u64,
    __unused: [u32; 2],
}

#[cfg(target_arch = "riscv32")]
#[repr(C)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad1: u64,
    st_size: i64,
    st_blksize: i32,
    __pad2: i32,
    st_blocks: i64,
    st_atime: i64,
    st_atime_nsec: u64,
    st_mtime: i64,
    st_mtime_nsec: u64,
    st_ctime: i64,
    st_ctime_nsec: u64,
    __unused: [u32; 2],
}

impl From<syscalls::Errno> for Error {
    fn from(value: syscalls::Errno) -> Self {
        #[cfg(feature = "std")]
        {
            Error::IO(std::io::Error::from_raw_os_error(value.into_raw()))
        }
        #[cfg(not(feature = "std"))]
        {
            Error::IO(value.to_string())
        }
    }
}

fn io_error(message: &'static str) -> Error {
    #[cfg(feature = "std")]
    {
        Error::IO(std::io::Error::new(std::io::ErrorKind::Other, message))
    }
    #[cfg(not(feature = "std"))]
    {
        Error::IO(alloc::string::String::from(message))
    }
}

pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
    read_file_limit(path, usize::MAX)
}

pub(crate) fn read_file_limit(path: &str, limit: usize) -> Result<Box<[u8]>> {
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

        if file_size > 0
            && unsafe { syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_SET) }
                .is_ok()
        {
            let read_size = core::cmp::min(file_size, limit);
            buffer.reserve_exact(read_size);
            unsafe {
                buffer.set_len(read_size);
            }
            let bytes_read = unsafe {
                syscalls::syscall3(
                    syscalls::Sysno::read,
                    fd as usize,
                    buffer.as_mut_ptr() as usize,
                    read_size,
                )?
            };
            if bytes_read != read_size {
                return Err(io_error("Failed to read complete file"));
            }
        } else {
            if file_size == 0 {
                let _ =
                    unsafe { syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_SET) };
            }
            let mut temp = [0u8; 1024];
            loop {
                let to_read = core::cmp::min(temp.len(), limit - buffer.len());
                if to_read == 0 {
                    break;
                }
                let bytes_read = unsafe {
                    syscalls::syscall3(
                        syscalls::Sysno::read,
                        fd as usize,
                        temp.as_mut_ptr() as usize,
                        to_read,
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

pub(crate) fn get_file_inode(path: &str) -> Result<FileIdentity> {
    let mut path_c = Vec::from(path.as_bytes());
    path_c.push(0);

    let mut stat_buf: LinuxStat = unsafe { core::mem::zeroed() };

    let result = unsafe {
        #[cfg(target_arch = "aarch64")]
        {
            syscalls::syscall4(
                syscalls::Sysno::fstatat,
                -100isize as usize, // AT_FDCWD
                path_c.as_ptr() as usize,
                &mut stat_buf as *mut _ as usize,
                0, // flags
            )
        }
        #[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
        {
            syscalls::syscall4(
                syscalls::Sysno::newfstatat,
                -100isize as usize, // AT_FDCWD
                path_c.as_ptr() as usize,
                &mut stat_buf as *mut _ as usize,
                0, // flags
            )
        }
        #[cfg(target_arch = "x86_64")]
        {
            syscalls::syscall2(
                syscalls::Sysno::stat,
                path_c.as_ptr() as usize,
                &mut stat_buf as *mut _ as usize,
            )
        }
    };

    match result {
        Ok(_) => Ok(FileIdentity {
            dev: stat_buf.st_dev as u64,
            ino: stat_buf.st_ino as u64,
        }),
        Err(e) => Err(Error::from(e)),
    }
}
