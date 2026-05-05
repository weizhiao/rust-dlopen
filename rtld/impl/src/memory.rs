#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8 {
    let mut index = 0;
    while index < len {
        unsafe { dst.add(index).write(src.add(index).read()) };
        index += 1;
    }
    dst
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, len: usize) -> *mut u8 {
    if (dst as usize) <= (src as usize) {
        unsafe { memcpy(dst, src, len) }
    } else {
        let mut index = len;
        while index != 0 {
            index -= 1;
            unsafe { dst.add(index).write(src.add(index).read()) };
        }
        dst
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut u8, value: i32, len: usize) -> *mut u8 {
    let mut index = 0;
    while index < len {
        unsafe { dst.add(index).write(value as u8) };
        index += 1;
    }
    dst
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(left: *const u8, right: *const u8, len: usize) -> i32 {
    let mut index = 0;
    while index < len {
        let lhs = unsafe { left.add(index).read() };
        let rhs = unsafe { right.add(index).read() };
        if lhs != rhs {
            return lhs as i32 - rhs as i32;
        }
        index += 1;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(left: *const u8, right: *const u8, len: usize) -> i32 {
    unsafe { memcmp(left, right, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(mut ptr: *const u8) -> usize {
    let start = ptr;
    while unsafe { ptr.read() } != 0 {
        ptr = unsafe { ptr.add(1) };
    }
    (ptr as usize).wrapping_sub(start as usize)
}
