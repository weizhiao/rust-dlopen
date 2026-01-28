use crate::Result;
use crate::error::parse_ld_cache_error;
use crate::utils::debug::GDBDebug;
use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use libc::{AT_BASE, AT_PHDR, AT_PHNUM, O_RDONLY, SEEK_END, SEEK_SET, c_char};

pub(crate) unsafe fn get_r_debug() -> *mut GDBDebug {
    let phdr_addr = unsafe { libc::getauxval(AT_PHDR) as usize };
    let phnum = unsafe { libc::getauxval(AT_PHNUM) as usize };
    let base = unsafe { libc::getauxval(AT_BASE) as usize };

    unsafe { crate::os::find_r_debug(phdr_addr, phnum, base) }
}

pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
    let mut path_c = Vec::from(path.as_bytes());
    path_c.push(0);

    unsafe {
        let fd = libc::open(path_c.as_ptr() as *const c_char, O_RDONLY);
        if fd < 0 {
            return Err(parse_ld_cache_error(format!("Failed to open file: {path}")));
        }

        let res = (|| {
            let size = libc::lseek(fd, 0, SEEK_END);
            if size < 0 || libc::lseek(fd, 0, SEEK_SET) < 0 {
                return Err(parse_ld_cache_error("Failed to seek file"));
            }

            let mut buffer = Vec::new();
            if size > 0 {
                buffer.reserve_exact(size as usize);
                buffer.set_len(size as usize);
                if libc::read(fd, buffer.as_mut_ptr() as *mut _, size as usize) != size as _ {
                    return Err(parse_ld_cache_error("Failed to read complete file"));
                }
            } else {
                let mut temp = [0u8; 1024];
                loop {
                    let n = libc::read(fd, temp.as_mut_ptr() as *mut _, temp.len());
                    if n < 0 {
                        return Err(parse_ld_cache_error("Read error"));
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
