use crate::api::dl_iterate_phdr::CDlPhdrInfo;
use crate::core_impl::types::{ARGC, ARGV, ENVP, ExtraData, LinkMap};
use crate::os::get_r_debug;
use crate::utils::debug::GDBDebug;
use crate::{
    OpenFlags, Result,
    api::dl_iterate_phdr::CallBack,
    core_impl::loader::{DylibExt, LoadedDylib},
    core_impl::register::{DylibState, MANAGER, register},
};
use alloc::{borrow::ToOwned, boxed::Box, ffi::CString, vec::Vec};
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    ptr::{NonNull, null_mut},
    sync::atomic::{AtomicBool, Ordering},
};
use elf_loader::elf::abi::{
    DT_FINI, DT_FINI_ARRAY, DT_GNU_HASH, DT_GNU_LIBLIST, DT_HASH, DT_INIT, DT_INIT_ARRAY,
    DT_JMPREL, DT_NULL, DT_PLTGOT, DT_REL, DT_RELA, DT_RELACOUNT, DT_STRTAB, DT_SYMTAB, DT_VERDEF,
    DT_VERNEED, DT_VERSYM, PT_DYNAMIC, PT_LOAD, PT_TLS,
};
use elf_loader::{
    elf::{ElfDyn, ElfHeader, ElfPhdr},
    tls::DefaultTlsResolver,
};
use spin::Once;

const DT_RELR: i64 = 36;

#[inline]
fn get_debug_struct() -> Option<&'static mut GDBDebug> {
    unsafe {
        let ptr = get_r_debug();
        if !ptr.is_null() && (*ptr).version != 0 {
            return Some(&mut *ptr);
        }
    }
    None
}

/// A list of dynamic tags that contain absolute addresses that need to be recovered.
/// These addresses are often modified by the dynamic linker (like glibc) to be absolute,
/// but we need them to be relative to the base address for our own loader.
const DT_ADDR_TAGS: &[i64] = &[
    DT_PLTGOT as i64,
    DT_HASH as i64,
    DT_STRTAB as i64,
    DT_SYMTAB as i64,
    DT_RELA as i64,
    DT_INIT as i64,
    DT_FINI as i64,
    DT_REL as i64,
    DT_JMPREL as i64,
    DT_INIT_ARRAY as i64,
    DT_FINI_ARRAY as i64,
    DT_RELR,
    DT_GNU_HASH as i64,
    DT_GNU_LIBLIST as i64,
    DT_RELACOUNT as i64,
    DT_VERSYM as i64,
    DT_VERDEF as i64,
    DT_VERNEED as i64,
];

static ONCE: Once = Once::new();
static IS_MUSL: AtomicBool = AtomicBool::new(false);

/// Recovers the dynamic table by making absolute addresses relative to the base address.
/// This is necessary because some dynamic linkers (like glibc) modify the dynamic table in place.
unsafe fn recover_dynamic_table(dynamic_ptr: *const ElfDyn, base: usize) -> Vec<ElfDyn> {
    let mut count = 0;
    while unsafe { (*dynamic_ptr.add(count)).d_tag != DT_NULL as _ } {
        count += 1;
    }
    let mut table = (0..=count) // include DT_NULL
        .map(|i| unsafe { core::ptr::read(dynamic_ptr.add(i)) })
        .collect::<Vec<_>>();

    for entry in table.iter_mut() {
        if DT_ADDR_TAGS.contains(&(entry.d_tag as i64)) && (entry.d_un as usize) > base {
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
    table
}

unsafe fn from_raw(
    name: CString,
    base: usize,
    dynamic_ptr: *const ElfDyn,
    extra: Option<(&'static [ElfPhdr], Option<NonNull<u8>>, Option<usize>)>,
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
    let name_str = name.to_string_lossy().into_owned();
    user_data.c_name = Some(name);

    // 1. Initialize LinkMap
    let mut link_map = Box::new(if !host_link_map.is_null() {
        unsafe { *host_link_map }
    } else {
        LinkMap {
            l_addr: base as _,
            l_ld: dynamic_ptr as *mut _,
            l_name: null_mut(),
            l_next: null_mut(),
            l_prev: null_mut(),
        }
    });

    link_map.l_name = user_data.c_name.as_ref().unwrap().as_ptr();
    link_map.l_next = null_mut();
    link_map.l_prev = null_mut();
    user_data.link_map = Some(link_map);

    // 2. Recover dynamic table (glibc modifies it in place)
    if !name_str.contains("linux-vdso.so.1") && !IS_MUSL.load(Ordering::Relaxed) {
        let table = unsafe { recover_dynamic_table(dynamic_ptr, base) };
        user_data.dynamic_table = Some(table.into_boxed_slice());
    }

    // 3. Process phdrs and memory length
    let (phdrs, mut len) = get_phdrs_and_len(base, extra.map(|e| e.0));
    let mut use_phdrs = phdrs.to_vec();

    if let Some(table) = &user_data.dynamic_table {
        if let Some(p) = use_phdrs.iter_mut().find(|p| p.p_type == PT_DYNAMIC) {
            let offset = (table.as_ptr() as usize).wrapping_sub(base);
            p.p_vaddr = offset as u64;
        }
    }

    // Filter out PT_TLS if we don't have tls_data (e.g. from host dl_iterate_phdr with null dlpi_tls_data)
    if extra.as_ref().map_or(true, |e| e.1.is_none()) {
        use_phdrs.retain(|p| p.p_type != PT_TLS);
    }

    len = (len + 0xfff) & !0xfff; // align to page size

    log::info!(
        "from_raw: calling RelocatedDylib::new_unchecked, len={:#x}",
        len
    );

    let lib = unsafe {
        LoadedDylib::new_unchecked::<DefaultTlsResolver>(
            name_str.clone(),
            use_phdrs.as_slice(),
            (base as *mut c_void, len),
            |_ptr, _len| Ok(()),
            extra.and_then(|e| e.2).map(|o| -(o as isize)),
            user_data,
        )
    }
    .map_err(|e| {
        log::error!("from_raw: new_unchecked failed for [{}]: {:?}", name_str, e);
        e
    })?;

    Ok(Some(lib))
}

fn get_phdrs_and_len(base: usize, extra: Option<&[ElfPhdr]>) -> (&[ElfPhdr], usize) {
    let phdrs = extra.unwrap_or_else(|| {
        let ehdr = unsafe { &*(base as *const ElfHeader) };
        unsafe {
            core::slice::from_raw_parts(
                (base + ehdr.e_phoff as usize) as *const ElfPhdr,
                ehdr.e_phnum as usize,
            )
        }
    });

    let len = phdrs
        .iter()
        .filter(|phdr| phdr.p_type == PT_LOAD)
        .map(|phdr| (phdr.p_vaddr + phdr.p_memsz) as usize)
        .max()
        .unwrap_or(0);

    (phdrs, len)
}

fn find_host_link_map(base: usize) -> *mut LinkMap {
    let debug = crate::utils::debug::DEBUG.lock();
    let mut cur = if debug.debug.is_null() {
        null_mut()
    } else {
        unsafe { (*debug.debug).map }
    };
    while !cur.is_null() {
        if unsafe { (*cur).l_addr as usize == base } {
            return cur;
        }
        cur = unsafe { (*cur).l_next };
    }
    null_mut()
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

/// Initializes global variables (ARGC, ARGV, ENVP) from a given library's symbols.
fn update_libc_globals(lib: &LoadedDylib) {
    let inner = lib;
    unsafe {
        if ARGC == 0 {
            if let Some(s) = inner
                .get::<*const c_int>("__libc_argc")
                .or_else(|| inner.get::<*const c_int>("__argc"))
            {
                *core::ptr::addr_of_mut!(ARGC) = (**s) as usize;
            }
        }
        if ARGV.is_null() {
            if let Some(s) = inner
                .get::<*const *mut c_char>("__libc_argv")
                .or_else(|| inner.get::<*const *mut c_char>("__argv"))
            {
                *core::ptr::addr_of_mut!(ARGV) = *s;
            }
        }
        if ENVP.is_null() {
            if let Some(s) = inner
                .get::<*const *const *const c_char>("environ")
                .or_else(|| inner.get::<*const *const *const c_char>("__environ"))
            {
                *core::ptr::addr_of_mut!(ENVP) = **s;
            }
        }
    }
}

fn iterate_phdr(start: *mut LinkMap, mut f: impl FnMut(IterPhdr)) {
    log::info!("iterate_phdr: start={:?}", start);
    if start.is_null() {
        log::warn!("iterate_phdr: start is NULL, skipping initialization");
        return;
    }

    let mut iter_phdr = None;
    for cur_map in (LinkMapIter { current: start }) {
        let name_ptr = if cur_map.l_name.is_null() {
            b"\0".as_ptr() as *const c_char
        } else {
            cur_map.l_name
        };
        let name = unsafe { CStr::from_ptr(name_ptr) };
        let ns = name.to_string_lossy();

        if ns.contains("ld-musl") {
            IS_MUSL.store(true, Ordering::Relaxed);
        }

        let is_main = ns.is_empty();
        let is_libc = (ns.contains("libc") || ns.contains("ld-musl")) && ns.contains(".so");

        if is_main || is_libc {
            let lib = unsafe {
                from_raw(
                    name.to_owned(),
                    cur_map.l_addr as _,
                    cur_map.l_ld,
                    None,
                    cur_map as *const _ as *mut _,
                )
            }
            .ok()
            .flatten();

            if let Some(lib) = lib {
                update_libc_globals(&lib);

                if is_libc && iter_phdr.is_none() {
                    if let Some(iter) = unsafe { lib.get::<IterPhdr>("dl_iterate_phdr") } {
                        log::info!("iterate_phdr: found [{}] and its dl_iterate_phdr", ns);
                        iter_phdr = Some(*iter);
                    }
                }
            }
        }
    }

    f(iter_phdr.expect("iterate_phdr: could not find libc with dl_iterate_phdr"));
}

unsafe extern "C" fn callback(info: *mut CDlPhdrInfo, _size: usize, _data: *mut c_void) -> c_int {
    let info = unsafe { &*info };
    let base = info.dlpi_addr;
    let phdrs = unsafe { core::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
    let dynamic_ptr = phdrs
        .iter()
        .find(|p| p.p_type == PT_DYNAMIC)
        .map(|p| (base + p.p_vaddr as usize) as *const ElfDyn)
        .expect("No PT_DYNAMIC found in phdrs");

    // Calculate static TLS offset if applicable
    let static_offset = (!info.dlpi_tls_data.is_null()).then(|| {
        (DefaultTlsResolver::get_thread_pointer() as usize)
            .wrapping_sub(info.dlpi_tls_data as usize)
    });

    let lib = unsafe {
        from_raw(
            CStr::from_ptr(info.dlpi_name).to_owned(),
            base,
            dynamic_ptr,
            Some((
                phdrs,
                NonNull::new(info.dlpi_tls_data as *mut _),
                static_offset,
            )),
            find_host_link_map(base),
        )
    }
    .unwrap()
    .expect("from_raw failed in callback");

    log::info!(
        "Initialize lib: [{}] @ [{:#x}]",
        lib.shortname(),
        lib.base()
    );
    register(
        lib,
        OpenFlags::RTLD_NODELETE | OpenFlags::RTLD_GLOBAL,
        &mut *crate::lock_write!(MANAGER),
        *DylibState::default().set_relocated(),
    );
    0
}

pub fn init() {
    log::info!("init: starting initialization");
    ONCE.call_once(|| {
        if let Some(debug) = get_debug_struct() {
            crate::utils::debug::init_debug(debug);
            iterate_phdr(debug.map, |iter| {
                iter(Some(callback), null_mut());
            });

            // Compute deps for all host libraries
            let mut lock = crate::lock_write!(MANAGER);
            let names: Vec<_> = lock.all.keys().cloned().collect();
            crate::core_impl::register::update_dependency_scopes(&mut lock, &names);
        }
        log::info!("init: initialization complete");
    });
}
