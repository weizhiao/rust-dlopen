use crate::Result;
use crate::utils::debug::GDBDebug;
use alloc::boxed::Box;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use libc::{AT_BASE, AT_PHDR, AT_PHNUM};

    pub(crate) unsafe fn get_r_debug() -> *mut GDBDebug {
        let phdr_addr = unsafe { libc::getauxval(AT_PHDR) as usize };
        let phnum = unsafe { libc::getauxval(AT_PHNUM) as usize };
        let base = unsafe { libc::getauxval(AT_BASE) as usize };

        unsafe { crate::os::find_r_debug(phdr_addr, phnum, base) }
    }
}

#[cfg(not(target_os = "linux"))]
mod generic {
    pub(crate) unsafe fn get_r_debug() -> *mut crate::utils::debug::GDBDebug {
        core::ptr::null_mut()
    }
}

#[cfg(feature = "std")]
mod std_impl {
    use super::*;

    pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
        std::fs::read(path)
            .map(|v| v.into_boxed_slice())
            .map_err(crate::Error::from)
    }

    pub(crate) fn read_file_limit(path: &str, limit: usize) -> Result<Box<[u8]>> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut buf = alloc::vec![0; limit];
        let n = file.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf.into_boxed_slice())
    }
}

#[cfg(not(feature = "std"))]
mod no_std_impl {
    use super::*;
    use alloc::{format, vec::Vec};
    use libc::{O_RDONLY, SEEK_END, SEEK_SET, c_char};

    pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
        read_file_limit(path, usize::MAX)
    }

    pub(crate) fn read_file_limit(path: &str, limit: usize) -> Result<Box<[u8]>> {
        let mut path_c = Vec::from(path.as_bytes());
        path_c.push(0);

        unsafe {
            let fd = libc::open(path_c.as_ptr() as *const c_char, O_RDONLY);
            if fd < 0 {
                return Err(crate::Error::IO(format!("Failed to open file: {path}")));
            }

            let res = (|| {
                let size = libc::lseek(fd, 0, SEEK_END);
                let mut buffer = Vec::new();

                if size > 0 && libc::lseek(fd, 0, SEEK_SET) >= 0 {
                    let read_size = core::cmp::min(size as usize, limit);
                    buffer.reserve_exact(read_size);
                    buffer.set_len(read_size);
                    let n = libc::read(fd, buffer.as_mut_ptr() as *mut _, read_size);
                    if n < 0 || n as usize != read_size {
                        return Err(crate::Error::IO(alloc::string::String::from(
                            "Failed to read complete file",
                        )));
                    }
                } else {
                    // Fallback for non-seekable files or files with 0 size (like /proc)
                    if size == 0 {
                        let _ = libc::lseek(fd, 0, SEEK_SET);
                    }
                    let mut temp = [0u8; 1024];
                    loop {
                        let to_read = core::cmp::min(temp.len(), limit - buffer.len());
                        if to_read == 0 {
                            break;
                        }
                        let n = libc::read(fd, temp.as_mut_ptr() as *mut _, to_read);
                        if n < 0 {
                            return Err(crate::Error::IO(alloc::string::String::from(
                                "Read error",
                            )));
                        }
                        if n == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&temp[..n as usize]);
                    }
                }
                Ok(buffer.into_boxed_slice())
            })();

            libc::close(fd);
            res
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) use generic::get_r_debug;
#[cfg(target_os = "linux")]
pub(crate) use linux::get_r_debug;

#[cfg(not(feature = "std"))]
pub(crate) use no_std_impl::*;
#[cfg(feature = "std")]
pub(crate) use std_impl::*;
