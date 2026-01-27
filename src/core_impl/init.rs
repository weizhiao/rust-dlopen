use crate::api::dl_iterate_phdr::CDlPhdrInfo;
use crate::arch::tp;
use crate::core_impl::types::{ARGC, ARGV, ENVP, LinkMap, ExtraData};
use crate::utils::debug::GDBDebug;
use crate::{
    OpenFlags, Result,
    api::dl_iterate_phdr::CallBack,
    core_impl::loader::{DylibExt, LoadedDylib},
    core_impl::register::{DylibState, MANAGER, register},
};
use alloc::{borrow::ToOwned, boxed::Box, ffi::CString, string::String, vec::Vec};
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    ptr::{NonNull, null_mut},
};
use elf_loader::elf::abi::{PT_DYNAMIC, PT_LOAD};
use elf_loader::{
    elf::{ElfDyn, ElfHeader, ElfPhdr},
    image::Symbol,
    tls::DefaultTlsResolver,
};
use spin::Once;

unsafe extern "C" {
    static _DYNAMIC: [ElfDyn; 0];
}

#[inline]
fn get_debug_struct() -> Option<&'static mut GDBDebug> {
    unsafe {
        let mut cur = _DYNAMIC.as_ptr();
        while !cur.is_null() && (*cur).d_tag != 0 {
            if (*cur).d_tag == 21 {
                // DT_DEBUG
                let ptr = (*cur).d_un as *mut GDBDebug;
                if !ptr.is_null() && (*ptr).version != 0 {
                    return Some(&mut *ptr);
                }
            }
            cur = cur.add(1);
        }
    }
    None
}

/// A list of dynamic tags that contain absolute addresses that need to be recovered.
/// These addresses are often modified by the dynamic linker (like glibc) to be absolute,
/// but we need them to be relative to the base address for our own loader.
const DT_ADDR_TAGS: &[i64] = &[
    3,          // DT_PLTGOT
    4,          // DT_HASH
    5,          // DT_STRTAB
    6,          // DT_SYMTAB
    7,          // DT_RELA
    12,         // DT_INIT
    13,         // DT_FINI
    17,         // DT_REL
    23,         // DT_JMPREL
    25,         // DT_INIT_ARRAY
    26,         // DT_FINI_ARRAY
    36,         // DT_RELR
    0x6ffffef5, // DT_GNU_HASH
    0x6ffffef4, // DT_TLSDESC_PLT
    0x6ffffff9, // DT_RELACOUNT
    0x6ffffff0, // DT_VERSYM
    0x6ffffffc, // DT_VERDEF
    0x6ffffffe, // DT_VERNEED
];

static ONCE: Once = Once::new();

/// Recovers the dynamic table by making absolute addresses relative to the base address.
/// This is necessary because some dynamic linkers (like glibc) modify the dynamic table in place.
unsafe fn recover_dynamic_table(dynamic_ptr: *const ElfDyn, base: usize) -> Vec<ElfDyn> {
    unsafe {
        let mut count = 0;
        let mut cur = dynamic_ptr;
        while !cur.is_null() && (*cur).d_tag != 0 {
            count += 1;
            cur = cur.add(1);
        }
        count += 1; // for DT_NULL

        let mut table = Vec::with_capacity(count);
        for i in 0..count {
            table.push(core::ptr::read(dynamic_ptr.add(i)));
        }

        for entry in table.iter_mut() {
            if DT_ADDR_TAGS.contains(&(entry.d_tag as i64)) {
                if (entry.d_un as usize) > base {
                    let old = entry.d_un;
                    entry.d_un = (entry.d_un as usize - base) as u64;
                    log::trace!(
                        "Recovered tag {}: {:#x} -> {:#x}",
                        entry.d_tag,
                        old,
                        entry.d_un
                    );
                }
            }
        }
        table
    }
}

unsafe fn from_raw(
    name: CString,
    base: usize,
    dynamic_ptr: *const ElfDyn,
    extra: Option<(&'static [ElfPhdr], Option<NonNull<u8>>, Option<usize>)>,
    add_debug: bool,
    host_link_map: *mut LinkMap,
) -> Result<Option<LoadedDylib>> {
    log::info!(
        "from_raw: name={:?}, base={:#x}, dynamic_ptr={:?}, host_link_map={:?}",
        name,
        base,
        dynamic_ptr,
        host_link_map
    );
    if dynamic_ptr.is_null() {
        log::info!("from_raw: dynamic_ptr is NULL, skipping");
        return Ok(None);
    }

    let mut user_data = ExtraData::new();
    user_data.c_name = Some(name);

    // 1. 初始化 LinkMap
    let mut link_map = if !host_link_map.is_null() {
        unsafe { *host_link_map }
    } else {
        LinkMap {
            l_addr: base as _,
            l_name: null_mut(),
            l_ld: dynamic_ptr as *mut _,
            l_next: null_mut(),
            l_prev: null_mut(),
        }
    };
    // 强制更新指针稳定性
    link_map.l_name = user_data.c_name.as_ref().unwrap().as_ptr();
    link_map.l_next = null_mut();
    link_map.l_prev = null_mut();

    let mut link_map = Box::new(link_map);
    let link_map_ptr = link_map.as_mut() as *mut LinkMap;
    user_data.link_map = Some(link_map);

    if add_debug {
        unsafe { crate::utils::debug::add_debug_link_map(link_map_ptr) };
    }

    // 2. 恢复动态表（glibc 会原位修改地址）
    if !user_data
        .c_name
        .as_ref()
        .unwrap()
        .to_string_lossy()
        .contains("linux-vdso.so.1")
    {
        let table = unsafe { recover_dynamic_table(dynamic_ptr, base) };
        user_data.dynamic_table = Some(table.into_boxed_slice());
    }

    // 3. 处理程序头和长度
    let (phdrs, mut len) = get_phdrs_and_len(base, extra.map(|e| e.0));

    // 如果有恢复的动态表，需要劫持 PT_DYNAMIC
    let use_phdrs = if let Some(table) = &user_data.dynamic_table {
        let mut v = phdrs.to_vec();
        if let Some(p) = v.iter_mut().find(|p| p.p_type == PT_DYNAMIC) {
            p.p_vaddr = (table.as_ptr() as usize).wrapping_sub(base) as u64;
        }
        Box::leak(v.into_boxed_slice()) as &'static _
    } else {
        // 确保生命周期为 'static
        let v = phdrs.to_vec();
        Box::leak(v.into_boxed_slice()) as &'static _
    };

    len = (len + 0xfff) & !0xfff;
    if len > usize::MAX - base {
        len = usize::MAX - base;
    }

    log::info!(
        "from_raw: calling RelocatedDylib::new_unchecked, len={:#x}",
        len
    );

    unsafe fn no_munmap(_ptr: *mut c_void, _len: usize) -> elf_loader::Result<()> {
        Ok(())
    }

    let lib = unsafe {
        LoadedDylib::new_unchecked::<DefaultTlsResolver>(
            user_data
                .c_name
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            use_phdrs,
            (base as *mut c_void, len),
            no_munmap,
            extra.and_then(|e| e.2).map(|o| -(o as isize)),
            user_data,
        )?
    };
    Ok(Some(lib))
}

fn get_phdrs_and_len(base: usize, extra: Option<&[ElfPhdr]>) -> (&[ElfPhdr], usize) {
    let phdrs = if let Some(p) = extra {
        p
    } else {
        let ehdr = unsafe { &*(base as *const ElfHeader) };
        unsafe {
            core::slice::from_raw_parts(
                (base + ehdr.e_phoff as usize) as *const ElfPhdr,
                ehdr.e_phnum as usize,
            )
        }
    };

    let len = phdrs
        .iter()
        .filter(|phdr| phdr.p_type == PT_LOAD)
        .map(|phdr| phdr.p_vaddr as usize + phdr.p_memsz as usize)
        .max()
        .unwrap_or(0);

    (phdrs, len)
}

fn find_host_link_map(base: usize) -> *mut LinkMap {
    let debug = crate::utils::debug::DEBUG.lock();
    let mut cur = if !debug.debug.is_null() {
        unsafe { (*debug.debug).map }
    } else {
        core::ptr::null_mut()
    };
    while !cur.is_null() {
        if unsafe { (*cur).l_addr as usize == base } {
            return cur;
        }
        cur = unsafe { (*cur).l_next };
    }
    core::ptr::null_mut()
}

type IterPhdr = extern "C" fn(callback: Option<CallBack>, data: *mut c_void) -> c_int;

struct LinkMapIter {
    current: *mut LinkMap,
}

impl Iterator for LinkMapIter {
    type Item = &'static LinkMap;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current.is_null() {
            None
        } else {
            let res = unsafe { &*self.current };
            self.current = res.l_next;
            Some(res)
        }
    }
}

fn iterate_phdr(start: *mut LinkMap, mut f: impl FnMut(Symbol<IterPhdr>)) {
    log::info!("iterate_phdr: start={:?}", start);
    if start.is_null() {
        log::warn!("iterate_phdr: start is NULL, skipping initialization");
        return;
    }

    for cur_map in (LinkMapIter { current: start }) {
        if cur_map.l_name.is_null() {
            continue;
        }
        let name = unsafe { CStr::from_ptr(cur_map.l_name) };
        let name_str = name.to_string_lossy();
        if name_str.is_empty() {
            continue;
        }

        // 我们只需要找到 libc.so 来获取 dl_iterate_phdr
        // 一旦找到并成功调用，就可以停止搜索
        if name_str.contains("libc.so") {
            let lib = unsafe {
                from_raw(
                    name.to_owned(),
                    cur_map.l_addr as usize,
                    cur_map.l_ld,
                    None,
                    false,
                    cur_map as *const _ as *mut _,
                )
                .ok()
                .flatten()
            };

            if let Some(lib) = lib {
                if let Some(iter) = unsafe { lib.get::<IterPhdr>("dl_iterate_phdr") } {
                    log::info!("iterate_phdr: found libc.so and its dl_iterate_phdr");
                    f(iter);
                    return;
                }
            }
        }
    }
    log::warn!("iterate_phdr: could not find libc.so with dl_iterate_phdr");
}

unsafe extern "C" fn callback(info: *mut CDlPhdrInfo, _size: usize, _data: *mut c_void) -> c_int {
    let info = unsafe { &*info };
    let base = info.dlpi_addr;
    let phdrs = unsafe { core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
    let dynamic_ptr = phdrs
        .iter()
        .find_map(|phdr| {
            (phdr.p_type == PT_DYNAMIC).then_some((base + phdr.p_vaddr as usize) as *const ElfDyn)
        })
        .expect("No PT_DYNAMIC found in phdrs");

    let static_offset = (!info.dlpi_tls_data.is_null())
        .then(|| {
            let is_main = unsafe { CStr::from_ptr(info.dlpi_name).to_bytes().is_empty() };
            let has_static_tls_flag = || unsafe {
                let mut cur = dynamic_ptr;
                while !cur.is_null() && (*cur).d_tag != 0 {
                    if (*cur).d_tag as i64 == 30 && ((*cur).d_un & 0x10) != 0 {
                        return true;
                    }
                    cur = cur.add(1);
                }
                false
            };
            (is_main || has_static_tls_flag())
                .then(|| tp().wrapping_sub(info.dlpi_tls_data as usize))
        })
        .flatten();

    let host_link_map = find_host_link_map(base);

    let lib = unsafe {
        from_raw(
            CStr::from_ptr(info.dlpi_name).to_owned(),
            base,
            dynamic_ptr,
            Some((
                phdrs,
                NonNull::new(info.dlpi_tls_data as *mut u8),
                static_offset,
            )),
            false,
            host_link_map,
        )
    }
    .unwrap()
    .expect("from_raw failed in callback");

    let flags = OpenFlags::RTLD_NODELETE | OpenFlags::RTLD_GLOBAL;

    log::info!(
        "Initialize lib: [{}] @ [{:#x}]",
        lib.shortname(),
        lib.base()
    );

    let mut lock = crate::lock_write!(MANAGER);
    register(
        lib,
        flags,
        &mut lock,
        *DylibState::default().set_relocated(),
    );

    0
}

/// Initializes global variables (ARGC, ARGV, ENVP) from libc symbols if available.
fn init_libc_globals() {
    let lock = crate::lock_read!(MANAGER);
    let Some(libc) = lock
        .all
        .values()
        .find(|lib| lib.shortname().contains("libc.so"))
    else {
        return;
    };

    let inner = libc.dylib_ref();
    unsafe {
        if *core::ptr::addr_of!(ARGC) == 0 {
            if let Some(argc_symbol) = inner.get::<*const c_int>("__libc_argc") {
                *core::ptr::addr_of_mut!(ARGC) = (**argc_symbol) as usize;
            }
        }
        if core::ptr::addr_of!(ARGV).read().is_null() && *core::ptr::addr_of!(ARGC) > 0 {
            if let Some(argv_symbol) = inner.get::<*const *mut c_char>("__libc_argv") {
                *core::ptr::addr_of_mut!(ARGV) = *argv_symbol;
            }
        }
        if core::ptr::addr_of!(ENVP).read().is_null() {
            if let Some(envp_symbol) = inner.get::<*const *const *const c_char>("environ") {
                *core::ptr::addr_of_mut!(ENVP) = **envp_symbol;
            }
        }
    }
}

pub fn init() {
    log::info!("init: starting initialization");

    ONCE.call_once(|| {
        if let Some(debug) = get_debug_struct() {
            crate::utils::debug::init_debug(debug);

            log::info!("init: iterate_phdr starting with debug.map={:?}", debug.map);
            iterate_phdr(debug.map, |iter| {
                iter(Some(callback), core::ptr::null_mut());
            });

            // Compute deps for all host libraries
            {
                let mut lock = crate::lock_write!(MANAGER);
                let names: Vec<String> = lock.all.keys().cloned().collect();
                crate::core_impl::register::update_dependency_scopes(&mut lock, &names);
            }

            init_libc_globals();
        }

        log::info!("init: initialization complete");
    });
}
