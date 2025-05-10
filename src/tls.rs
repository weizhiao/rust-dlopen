use elf_loader::arch::{ElfPhdr, TLS_DTV_OFFSET};
use libc::{
    __errno_location, pthread_getspecific, pthread_key_create, pthread_key_delete, pthread_key_t,
    pthread_setspecific,
};
use std::{
    alloc::{Layout, dealloc, handle_alloc_error},
    mem::{MaybeUninit, size_of},
    os::raw::c_void,
};
use thread_register::{ModifyRegister, ThreadRegister};

#[repr(C)]
pub(crate) struct TlsIndex {
    ti_module: usize,
    ti_offset: usize,
}

struct TlsInner {
    image: *const u8,
    len: usize,
    layout: Layout,
    offset: usize,
    key: pthread_key_t,
}

pub(crate) struct ElfTls {
    inner: Box<TlsInner>,
}

const MAX_TLS_INDEX: usize = 4096;

impl ElfTls {
    pub(crate) fn new(phdr: &ElfPhdr, base: usize) -> Self {
        unsafe extern "C" fn dtor(ptr: *mut c_void) {
            if !ptr.is_null() {
                let layout = unsafe { ptr.cast::<Layout>().read() };
                unsafe { dealloc(ptr.cast(), layout) };
            }
        }

        let mut key = MaybeUninit::uninit();
        assert!(
            (unsafe { pthread_key_create(key.as_mut_ptr(), Some(dtor)) } == 0),
            "can not create tls"
        );
        let align = phdr.p_align as usize;
        let mut size = (size_of::<Layout>() + align - 1) & !(align - 1);
        // 前面用来保存layout
        let offset = size;
        size += phdr.p_memsz as usize;
        let layout = unsafe { Layout::from_size_align_unchecked(size, align) };
        let inner = Box::new(TlsInner {
            image: unsafe { (base as *const u8).add(phdr.p_vaddr as usize) },
            len: phdr.p_filesz as usize,
            key: unsafe { key.assume_init() },
            layout,
            offset,
        });
        assert!((inner.as_ref() as *const _ as usize) > MAX_TLS_INDEX);
        Self { inner }
    }

    pub(crate) fn module_id(&self) -> usize {
        self.inner.as_ref() as *const TlsInner as usize
    }
}

impl Drop for ElfTls {
    fn drop(&mut self) {
        unsafe { pthread_key_delete(self.inner.key) };
    }
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn __tls_get_addr(tls_index: &TlsIndex) -> *const u8 {
    if tls_index.ti_module > MAX_TLS_INDEX {
        let tls = unsafe { &*(tls_index.ti_module as *const TlsInner) };
        let val = unsafe { pthread_getspecific(tls.key) };
        let data = if val.is_null() {
            let layout = tls.layout;
            let memory = unsafe { alloc::alloc::alloc_zeroed(layout) };
            if memory.is_null() {
                handle_alloc_error(layout);
            }
            unsafe { memory.cast::<Layout>().write(layout) };
            let data = unsafe { memory.add(tls.offset) };
            unsafe { data.copy_from_nonoverlapping(tls.image, tls.len) };
            if unsafe { pthread_setspecific(tls.key, memory.cast()) } != 0 {
                return core::ptr::null();
            }
            data
        } else {
            unsafe { val.add(tls.offset).cast() }
        };
        unsafe { data.add(tls_index.ti_offset.wrapping_add(TLS_DTV_OFFSET)) }
    } else {
        unsafe { TLS_GET_ADDR.unwrap()(tls_index) }
    }
}

// errno
static mut ERRNO_OFFSET: usize = 0;
// __resp
static mut __RESP_OFFSET: usize = 0;
// __h_errno
static mut __H_ERRNO_OFFSET: usize = 0;

pub(crate) static mut TLS_GET_ADDR: Option<unsafe extern "C" fn(&TlsIndex) -> *const u8> = None;

pub(crate) fn init_tls(resp: usize, h_errno: usize) {
    let errno = unsafe { __errno_location() } as usize;
    let tp_val = ThreadRegister::get();
    let (offset, _) = errno.overflowing_sub(tp_val);
    unsafe {
        ERRNO_OFFSET = offset;
        __RESP_OFFSET = resp.overflowing_sub(tp_val).0;
        __H_ERRNO_OFFSET = h_errno.overflowing_sub(tp_val).0;
        log::debug!(
            "errno: [{:#x}], __resp: [{:#x}], __h_errno: [{:#x}]",
            errno,
            resp,
            h_errno,
        );
    }
}

pub(crate) fn get_libc_tls_offset(name: &str) -> Option<usize> {
    unsafe {
        if name == "errno" {
            assert!(ERRNO_OFFSET != 0);
            Some(ERRNO_OFFSET)
        } else if name == "__resp" {
            assert!(__RESP_OFFSET != 0);
            Some(__RESP_OFFSET)
        } else if name == "__h_errno" {
            assert!(__H_ERRNO_OFFSET != 0);
            Some(__H_ERRNO_OFFSET)
        } else {
            None
        }
    }
}
