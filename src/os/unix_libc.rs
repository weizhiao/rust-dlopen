use crate::Result;
use crate::error::parse_ld_cache_error;
use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use libc::{c_char, O_RDONLY, SEEK_END, SEEK_SET};

pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
    let mut path_c = Vec::from(path.as_bytes());
    path_c.push(0);
    let path_ptr = path_c.as_ptr() as *const c_char;

    unsafe {
        let fd = libc::open(path_ptr, O_RDONLY);
        if fd < 0 {
            return Err(parse_ld_cache_error(format!("Failed to open file: {}", path)));
        }

        let file_size = libc::lseek(fd, 0, SEEK_END);
        if file_size < 0 {
            libc::close(fd);
            return Err(parse_ld_cache_error("Failed to seek to end of file"));
        }

        if libc::lseek(fd, 0, SEEK_SET) < 0 {
            libc::close(fd);
            return Err(parse_ld_cache_error("Failed to seek to start of file"));
        }

        let mut buffer = Vec::with_capacity(file_size as usize);
        buffer.set_len(file_size as usize);

        let bytes_read = libc::read(fd, buffer.as_mut_ptr() as *mut _, file_size as usize);
        libc::close(fd);

        if bytes_read != file_size as _ {
            return Err(parse_ld_cache_error("Failed to read complete file"));
        }

        Ok(buffer.into_boxed_slice())
    }
}
