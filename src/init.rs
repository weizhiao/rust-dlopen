use crate::{
    OpenFlags, Result,
    abi::CDlPhdrInfo,
    dl_iterate_phdr::CallBack,
    register::{DylibState, MANAGER, global_find, register},
};
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    num::NonZero,
    ptr::{NonNull, addr_of, addr_of_mut, null_mut},
};
use elf_loader::{
    RelocatedDylib, Symbol, UserData,
    abi::{PT_DYNAMIC, PT_LOAD},
    arch::{Dyn, ElfPhdr},
    dynamic::ElfDynamic,
    segment::{ElfSegments, MASK, PAGE_SIZE},
    set_global_scope,
};
use spin::Once;
use std::{env, ffi::CString, os::unix::ffi::OsStringExt, path::PathBuf, sync::Arc};

#[repr(C)]
pub(crate) struct LinkMap {
    pub l_addr: *mut c_void,
    pub l_name: *const c_char,
    pub l_ld: *mut Dyn,
    pub l_next: *mut LinkMap,
    pub l_prev: *mut LinkMap,
}

#[repr(C)]
pub(crate) struct GDBDebug {
    pub version: c_int,
    pub map: *mut LinkMap,
    pub brk: extern "C" fn(),
    pub state: c_int,
    pub ldbase: *mut c_void,
}

#[cfg(target_env = "gnu")]
#[inline]
fn get_debug_struct() -> &'static mut GDBDebug {
    unsafe extern "C" {
        static mut _r_debug: GDBDebug;
    }
    unsafe { &mut *addr_of_mut!(_r_debug) }
}

// 静态链接的musl中没有_dl_debug_addr这个符号，无法通过编译，因此需要生成dyn格式的可执行文件
#[cfg(target_env = "musl")]
#[inline]
fn get_debug_struct() -> &'static mut GDBDebug {
    unsafe extern "C" {
        static mut _dl_debug_addr: GDBDebug;
    }
    unsafe { &mut *addr_of_mut!(_dl_debug_addr) }
}

static ONCE: Once = Once::new();
static mut PROGRAM_NAME: Option<PathBuf> = None;

pub(crate) static mut ARGC: usize = 0;
pub(crate) static mut ARGV: Vec<*mut c_char> = Vec::new();
pub(crate) static mut ENVP: usize = 0;

unsafe extern "C" {
    static environ: usize;
}

fn create_segments(base: usize, len: usize) -> Option<ElfSegments> {
    let memory = if let Some(memory) = NonNull::new(base as _) {
        memory
    } else {
        // 如果程序本身不是Shared object file,那么它的这个字段为0,此时无法使用程序本身的符号进行重定位
        log::warn!(
            "Failed to initialize an existing library: [{:?}], Because it's not a Shared object file",
            unsafe { (*addr_of!(PROGRAM_NAME)).as_ref().unwrap() }
        );
        return None;
    };
    unsafe fn drop_handle(_handle: NonNull<c_void>, _len: usize) -> elf_loader::Result<()> {
        Ok(())
    }
    Some(ElfSegments::new(memory, len, drop_handle))
}

pub(crate) unsafe fn from_raw(
    name: CString,
    segments: ElfSegments,
    dynamic_ptr: *const Dyn,
    phdrs: Option<&'static [ElfPhdr]>,
) -> Result<Option<RelocatedDylib<'static>>> {
    #[allow(unused_mut)]
    let mut dynamic = ElfDynamic::new(dynamic_ptr, &segments)?;

    // 因为glibc会修改dynamic段中的信息，所以这里需要手动恢复一下。
    if !name.to_str().unwrap().contains("linux-vdso.so.1") {
        let base = segments.base();
        if dynamic.strtab > 2 * base {
            dynamic.strtab -= base;
            dynamic.symtab -= base;
            dynamic.hashtab -= base;
            dynamic.version_idx = dynamic
                .version_idx
                .map(|v| NonZero::new(v.get() - base).unwrap());
        }
    }

    #[allow(unused_mut)]
    let mut user_data = UserData::empty();
    #[cfg(feature = "debug")]
    unsafe {
        if phdrs.is_some() {
            use super::debug::*;
            user_data.insert(
                crate::loader::DEBUG_INFO_ID,
                Box::new(DebugInfo::new(
                    segments.base(),
                    name.as_ptr() as _,
                    dynamic_ptr as usize,
                )),
            );
        }
    };
    let len = if let Some(phdrs) = phdrs {
        let mut min_vaddr = usize::MAX;
        let mut max_vaddr = 0;
        phdrs.iter().for_each(|phdr| {
            if phdr.p_type == PT_LOAD {
                min_vaddr = min_vaddr.min(phdr.p_vaddr as usize & MASK);
                max_vaddr = max_vaddr
                    .max((phdr.p_vaddr as usize + phdr.p_memsz as usize + PAGE_SIZE - 1) & MASK);
            }
        });
        max_vaddr - min_vaddr
    } else {
        usize::MAX
    };
    let new_segments: ElfSegments = create_segments(segments.base(), len).unwrap();
    let lib = unsafe {
        RelocatedDylib::new_uncheck(
            name,
            new_segments.base(),
            dynamic,
            phdrs.unwrap_or(&[]),
            new_segments,
            user_data,
        )
    };
    Ok(Some(lib))
}

type IterPhdr = extern "C" fn(callback: Option<CallBack>, data: *mut c_void) -> c_int;

// 寻找libc中的dl_iterate_phdr函数
fn iterate_phdr(start: *const LinkMap, mut f: impl FnMut(Symbol<IterPhdr>)) {
    let mut cur_map_ptr = start;
	#[cfg(feature = "tls")]
    while !cur_map_ptr.is_null() {
        let cur_map = unsafe { &*cur_map_ptr };
        let name = unsafe { CStr::from_ptr(cur_map.l_name).to_owned() };
        let Some(segments) = create_segments(cur_map.l_addr as usize, usize::MAX) else {
            cur_map_ptr = cur_map.l_next;
            continue;
        };
        if let Some(lib) = unsafe { from_raw(name, segments, cur_map.l_ld, None).unwrap() } {
            if lib.name().contains("ld-") {
                let tls_get_addr = unsafe {
                    lib.get::<extern "C" fn(tls_index: &crate::loader::tls::TlsIndex) -> *const u8>(
                        "__tls_get_addr",
                    )
                    .unwrap()
                };
                unsafe { crate::loader::tls::TLS_GET_ADDR = Some(core::mem::transmute(tls_get_addr)) };
                break;
            }
        };
        cur_map_ptr = cur_map.l_next;
    }
	
	cur_map_ptr = start;
	while !cur_map_ptr.is_null() {
        let cur_map = unsafe { &*cur_map_ptr };
        let name = unsafe { CStr::from_ptr(cur_map.l_name).to_owned() };
        let Some(segments) = create_segments(cur_map.l_addr as usize, usize::MAX) else {
            cur_map_ptr = cur_map.l_next;
            continue;
        };
        if let Some(lib) = unsafe { from_raw(name, segments, cur_map.l_ld, None).unwrap() } {
            #[cfg(feature = "tls")]
            if lib.name().contains("ld-") {
                let tls_get_addr = unsafe {
                    lib.get::<extern "C" fn(tls_index: &crate::loader::tls::TlsIndex) -> *const u8>(
                        "__tls_get_addr",
                    )
                    .unwrap()
                };
                unsafe { crate::loader::tls::TLS_GET_ADDR = Some(core::mem::transmute(tls_get_addr)) };
                continue;
            }

            if lib.name().contains("libc.so") {
                f(unsafe { lib.get::<IterPhdr>("dl_iterate_phdr").unwrap() });
                #[cfg(feature = "tls")]
                {
                    let dlopen = unsafe {
                        lib.get::<extern "C" fn (filename: *const c_char, flags: c_int) -> *const c_void >("dlopen")
							.unwrap()
                    };
                    let libc_handle = dlopen(lib.cname().as_ptr(), 0);
                    let dlsym = unsafe {
                        lib.get::<extern "C" fn(*const c_void, *const c_char) -> *const c_void>(
                            "dlsym",
                        )
                        .unwrap()
                    };
                    crate::loader::tls::init_tls(
                        dlsym(libc_handle, c"__resp".as_ptr()) as _,
                        dlsym(libc_handle, c"__h_errno".as_ptr()) as _,
                    );
                }
                return;
            }
        };
        cur_map_ptr = cur_map.l_next;
    }
    panic!("can not find libc's dl_iterate_phdr");
}

fn init_argv() {
    let mut argv = Vec::new();
    for arg in env::args_os() {
        argv.push(CString::new(arg.into_vec()).unwrap().into_raw());
    }
    argv.push(null_mut());
    unsafe {
        ARGC = argv.len();
        ARGV = argv;
        ENVP = environ;
    }
}

unsafe extern "C" fn callback(info: *mut CDlPhdrInfo, _size: usize, _data: *mut c_void) -> c_int {
    let info = unsafe { &*info };
    let base = info.dlpi_addr;
    let phdrs = unsafe { core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
    let dynamic_ptr = phdrs
        .iter()
        .find_map(|phdr| {
            if phdr.p_type == PT_DYNAMIC {
                Some(base + phdr.p_vaddr as usize)
            } else {
                None
            }
        })
        .unwrap() as _;
    let Some(segments) = create_segments(base, usize::MAX) else {
        return 0;
    };
    let Some(lib) = unsafe {
        from_raw(
            CStr::from_ptr(info.dlpi_name).to_owned(),
            segments,
            dynamic_ptr,
            Some(phdrs),
        )
    }
    .unwrap() else {
        return 0;
    };
    let flags = OpenFlags::RTLD_NODELETE | OpenFlags::RTLD_GLOBAL;
    let deps = Some(Arc::new(vec![lib.clone()].into_boxed_slice()));
    let start = lib.base();
    let end = start + lib.map_len();
    let shortname = lib.shortname();
    let name = if shortname.is_empty() {
        unsafe {
            (*addr_of!(PROGRAM_NAME))
                .as_ref()
                .unwrap()
                .to_str()
                .unwrap()
        }
    } else {
        shortname
    };
    log::info!(
        "Initialize an existing library: [{:?}] [{:#x}]-[{:#x}]",
        name,
        start,
        end,
    );

    register(
        lib,
        flags,
        deps,
        &mut MANAGER.write(),
        *DylibState::default().set_relocated(),
    );
    0
}
/// `init` is responsible for the initialization of dlopen_rs, If you want to use the dynamic library that the program itself depends on,
/// or want to use the debug function, please call it at the beginning. This is usually necessary.
pub fn init() {
    ONCE.call_once(|| {
        init_argv();
        let program_self = env::current_exe().unwrap();
        unsafe { PROGRAM_NAME = Some(program_self) };
        let debug = get_debug_struct();
        iterate_phdr(debug.map, |iter| {
            #[cfg(feature = "debug")]
            crate::debug::init_debug(debug);
            iter(Some(callback), null_mut());
        });
        unsafe { set_global_scope(global_find as _) };
        log::info!("Initialization is complete");
    });
}
