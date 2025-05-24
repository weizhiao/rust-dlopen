use crate::tls::{
    DTV_OFFSET, TLS_GENERATION, TLS_STATIC_ALIGN, TLS_STATIC_SIZE, TlsState, add_tls, init_tls,
};
use crate::{
    OpenFlags, Result,
    abi::CDlPhdrInfo,
    dl_iterate_phdr::CallBack,
    loader::{EH_FRAME_ID, EhFrame},
    register::{DylibState, MANAGER, global_find, register},
};
use alloc::{borrow::ToOwned, boxed::Box, ffi::CString, sync::Arc, vec::Vec};
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    mem::ManuallyDrop,
    num::NonZero,
    ptr::{NonNull, addr_of_mut},
};
use elf_loader::{
    RelocatedDylib, Symbol, UserData,
    abi::{PT_DYNAMIC, PT_GNU_EH_FRAME, PT_LOAD, PT_TLS},
    arch::{Dyn, ElfPhdr},
    dynamic::ElfDynamic,
    segment::{ElfSegments, MASK, PAGE_SIZE},
    set_global_scope,
};
use spin::Once;
use thread_register::{ModifyRegister, ThreadRegister};

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
//static mut PROGRAM_NAME: Option<PathBuf> = None;

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
        // log::warn!(
        //     "Failed to initialize an existing library: [{:?}], Because it's not a Shared object file",
        //     unsafe { (*addr_of!(PROGRAM_NAME)).as_ref().unwrap() }
        // );
        return None;
    };
    unsafe fn drop_handle(_handle: NonNull<c_void>, _len: usize) -> elf_loader::Result<()> {
        Ok(())
    }
    Some(ElfSegments::new(memory, len, drop_handle))
}

unsafe fn from_raw(
    name: CString,
    segments: ElfSegments,
    dynamic_ptr: *const Dyn,
    extra: Option<(&'static [ElfPhdr], &StaticTlsInfo, usize)>,
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
        if extra.is_some() {
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
    let mut use_phdrs: &[ElfPhdr] = &[];
    let len = if let Some((phdrs, tls, modid)) = extra {
        let mut min_vaddr = usize::MAX;
        let mut max_vaddr = 0;
        phdrs.iter().for_each(|phdr| {
            if phdr.p_type == PT_LOAD {
                min_vaddr = min_vaddr.min(phdr.p_vaddr as usize & MASK);
                max_vaddr = max_vaddr
                    .max((phdr.p_vaddr as usize + phdr.p_memsz as usize + PAGE_SIZE - 1) & MASK);
            } else if phdr.p_type == PT_GNU_EH_FRAME {
                user_data.insert(
                    EH_FRAME_ID,
                    Box::new(EhFrame::new(phdr.p_vaddr as usize + segments.base())),
                );
            } else if phdr.p_type == PT_TLS {
                add_tls(
                    &segments,
                    phdr,
                    &mut user_data,
                    TlsState::Initialized(tls.get_offset(modid - 1)),
                );
            }
        });
        use_phdrs = phdrs;
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
            use_phdrs,
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
    let mut needed_libs = Vec::new();
    while !cur_map_ptr.is_null() {
        let cur_map = unsafe { &*cur_map_ptr };
        let name = unsafe { CStr::from_ptr(cur_map.l_name).to_owned() };
        let Some(segments) = create_segments(cur_map.l_addr as usize, usize::MAX) else {
            cur_map_ptr = cur_map.l_next;
            continue;
        };
        if let Some(lib) = unsafe { from_raw(name, segments, cur_map.l_ld, None).unwrap() } {
            let lib_name = lib.name();
            if lib_name.contains("libc.so") || lib_name.contains("ld-") {
                needed_libs.push(lib);
                // 目前只要用这两个
                if needed_libs.len() >= 2 {
                    break;
                }
            }
        };
        cur_map_ptr = cur_map.l_next;
    }
    assert!(needed_libs.len() == 2);
    for lib in needed_libs {
        if lib.name().contains("libc.so") {
            f(unsafe { lib.get::<IterPhdr>("dl_iterate_phdr").unwrap() });
        } else if lib.name().contains("ld-") {
            let mut tls_static_size: usize = 0;
            let mut tls_static_align: usize = 0;
            unsafe {
                lib.get::<extern "C" fn(*mut usize, *mut usize)>("_dl_get_tls_static_info")
                    .unwrap()(&mut tls_static_size, &mut tls_static_align);
            }
            unsafe { TLS_STATIC_SIZE = tls_static_size };
            unsafe { TLS_STATIC_ALIGN = tls_static_align };
            log::debug!(
                "tls static info: size: {}, align: {}",
                tls_static_size,
                tls_static_align
            )
        }
    }
}

fn init_argv() {
    // let mut argv = Vec::new();
    // for arg in env::args_os() {
    //     argv.push(CString::new(arg.into_vec()).unwrap().into_raw());
    // }
    // argv.push(null_mut());
    // unsafe {
    //     ARGC = argv.len();
    //     ARGV = argv;
    //     ENVP = environ;
    // }
}

#[repr(C)]
struct DtvPointer {
    val: *const c_void,
    free: *const c_void,
}

#[repr(C)]
union Dtv {
    counter: usize,
    pointer: ManuallyDrop<DtvPointer>,
}

struct StaticTlsInfo {
    dtv: &'static [Dtv],
    tcb: usize,
}

impl StaticTlsInfo {
    fn new() -> StaticTlsInfo {
        let tcb = ThreadRegister::base();
        let dtv = unsafe { ThreadRegister::get::<*const Dtv>(DTV_OFFSET) };
        let count = unsafe { dtv.sub(1).read().counter };
        let slice = unsafe { core::slice::from_raw_parts(dtv.add(1), count) };
        StaticTlsInfo { dtv: slice, tcb }
    }

    fn get_offset(&self, modid: usize) -> usize {
        unsafe { self.tcb.wrapping_sub(self.dtv[modid].pointer.val as usize) }
    }
}

unsafe extern "C" fn callback(info: *mut CDlPhdrInfo, _size: usize, data: *mut c_void) -> c_int {
    let info = unsafe { &*info };
    let static_tls_info = unsafe { &mut *(data as *mut StaticTlsInfo) };
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
            Some((phdrs, static_tls_info, info.dlpi_tls_modid)),
        )
    }
    .unwrap() else {
        return 0;
    };
    let flags = OpenFlags::RTLD_NODELETE | OpenFlags::RTLD_GLOBAL;
    let mut temp = Vec::new();
    temp.push(lib.clone());
    let deps = Some(Arc::new(temp.into_boxed_slice()));
    let start = lib.base();
    let end = start + lib.map_len();
    let shortname = lib.shortname();
    let name = if shortname.is_empty() {
        // unsafe {
        //     (*addr_of!(PROGRAM_NAME))
        //         .as_ref()
        //         .unwrap()
        //         .to_str()
        //         .unwrap()
        // }
        ""
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
        // let program_self = env::current_exe().unwrap();
        // unsafe { PROGRAM_NAME = Some(program_self) };
        let debug = get_debug_struct();
        iterate_phdr(debug.map, |iter| {
            #[cfg(feature = "debug")]
            crate::debug::init_debug(debug);
            let mut tls_info = StaticTlsInfo::new();
            iter(Some(callback), &mut tls_info as *const _ as *mut _);
            TLS_GENERATION.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        });
        init_tls();
        unsafe { set_global_scope(global_find as _) };
        log::info!("Initialization is complete");
    });
}
