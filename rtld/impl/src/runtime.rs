use core::panic::PanicInfo;
use core::{alloc::Layout, ptr::null_mut};

use syscalls::Sysno;

pub(crate) const RTLD_FATAL_EXIT_STATUS: usize = 127;

pub(crate) unsafe fn read_usize(ptr: *const usize) -> usize {
    unsafe { core::ptr::read(ptr) }
}

pub(crate) fn write_stderr(bytes: &[u8]) {
    write_fd(2, bytes);
}

pub(crate) fn write_stdout(bytes: &[u8]) {
    write_fd(1, bytes);
}

fn write_fd(fd: usize, bytes: &[u8]) {
    unsafe {
        let _ = syscalls::syscall3(Sysno::write, fd, bytes.as_ptr() as usize, bytes.len());
    }
}

pub(crate) fn exit(status: usize) -> ! {
    unsafe {
        let _ = syscalls::syscall1(Sysno::exit_group, status);
        let _ = syscalls::syscall1(Sysno::exit, status);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    exit(RTLD_FATAL_EXIT_STATUS)
}

struct RtldAllocator;

#[global_allocator]
static ALLOCATOR: RtldAllocator = RtldAllocator;

unsafe impl core::alloc::GlobalAlloc for RtldAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = page_rounded_size(layout);
        const PROT_READ: usize = 0x1;
        const PROT_WRITE: usize = 0x2;
        const MAP_PRIVATE: usize = 0x02;
        const MAP_ANONYMOUS: usize = 0x20;
        let Ok(ptr) = (unsafe {
            syscalls::syscall6(
                Sysno::mmap,
                0,
                size,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                usize::MAX,
                0,
            )
        }) else {
            return null_mut();
        };
        ptr as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = page_rounded_size(layout);
        let _ = unsafe { syscalls::syscall2(Sysno::munmap, ptr as usize, size) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Ok(new_layout) = Layout::from_size_align(new_size.max(1), layout.align()) else {
            return null_mut();
        };
        let new_ptr = unsafe { self.alloc(new_layout) };
        if new_ptr.is_null() {
            return null_mut();
        }

        let copy_len = layout.size().min(new_size);
        let mut offset = 0usize;
        while offset < copy_len {
            let byte = unsafe { ptr.add(offset).read_volatile() };
            unsafe { new_ptr.add(offset).write_volatile(byte) };
            offset = offset.wrapping_add(1);
        }

        unsafe { self.dealloc(ptr, layout) };
        new_ptr
    }
}

fn page_rounded_size(layout: Layout) -> usize {
    let size = layout.size().max(layout.align());
    (size + 0xfff) & !0xfff
}
