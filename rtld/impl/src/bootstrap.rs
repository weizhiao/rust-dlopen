use core::{
    ffi::c_void,
    fmt::{self, Write},
    ptr::{addr_of_mut, null, null_mut},
};
use dlopen_rs::rtld_abi::{
    auxv::{
        AT_BASE, AT_CLKTCK, AT_ENTRY, AT_FPUCW, AT_HWCAP, AT_HWCAP2, AT_HWCAP3, AT_HWCAP4,
        AT_MINSIGSTKSZ, AT_NULL, AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM, AT_PLATFORM, AT_SECURE,
        AT_SYSINFO_EHDR,
    },
    bootstrap::{BootstrapMode, BootstrapObject, BootstrapState},
    debug::{LinkMap, RDebug, RT_CONSISTENT},
    elf::{ElfDyn, ElfDynamicTag, ElfHeader, ElfPhdr, ElfProgramType, ElfRelType},
};
use dlopen_rs::rtld_stage1;

use crate::{
    cli::{DirectProgram, handle_direct_invocation},
    globals::{
        __libc_enable_secure, __libc_stack_end, _dl_argv, _r_debug, EMPTY_NAME, MAIN_LINK_MAP,
        RTLD_NAME, RtldGlobalRoAux, publish_rtld_globals, rtld_link_map,
    },
    runtime::{exit, read_usize, write_stderr},
};

#[derive(Copy, Clone)]
struct AuxState {
    phdr: usize,
    phent: usize,
    phnum: usize,
    base: usize,
    entry: usize,
    secure: usize,
    pagesize: usize,
    platform: usize,
    hwcap: usize,
    hwcap2: usize,
    hwcap3: usize,
    hwcap4: usize,
    clktck: usize,
    fpucw: usize,
    minsigstacksize: usize,
    sysinfo_ehdr: usize,
}

impl AuxState {
    const fn empty() -> Self {
        Self {
            phdr: 0,
            phent: 0,
            phnum: 0,
            base: 0,
            entry: 0,
            secure: 0,
            pagesize: 0,
            platform: 0,
            hwcap: 0,
            hwcap2: 0,
            hwcap3: 0,
            hwcap4: 0,
            clktck: 0,
            fpucw: 0,
            minsigstacksize: 0,
            sysinfo_ehdr: 0,
        }
    }

    fn phdrs(self) -> PhdrIter {
        PhdrIter {
            aux: self,
            index: 0,
        }
    }

    fn find_phdr(self, program_type: ElfProgramType) -> Option<ElfPhdr> {
        self.phdrs()
            .find(|phdr| phdr.program_type() == program_type)
    }

    fn phdr_at(self, index: usize) -> Option<ElfPhdr> {
        if index >= self.phnum || self.phdr == 0 || self.phent < core::mem::size_of::<ElfPhdr>() {
            return None;
        }

        let offset = index.wrapping_mul(self.phent);
        let ptr = (self.phdr as *const u8).wrapping_add(offset) as *const ElfPhdr;
        Some(unsafe { core::ptr::read_unaligned(ptr) })
    }
}

struct PhdrIter {
    aux: AuxState,
    index: usize,
}

impl Iterator for PhdrIter {
    type Item = ElfPhdr;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;
        let Some(phdr) = self.aux.phdr_at(index) else {
            self.index = self.aux.phnum;
            return None;
        };
        self.index = self.index.wrapping_add(1);
        Some(phdr)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rtld_bootstrap(stack: *const usize, rtld_dynamic: *const usize) -> usize {
    let argc = unsafe { read_usize(stack) };
    let argv = unsafe { stack.add(1) as *const *const u8 };

    let envp_start = unsafe { stack.add(argc.wrapping_add(2)) };
    let mut envp = envp_start;
    while unsafe { read_usize(envp) } != 0 {
        envp = unsafe { envp.add(1) };
    }

    let auxv = unsafe { envp.add(1) };
    let aux = unsafe { parse_auxv(auxv) };
    let kernel_load_bias = main_load_bias(aux);
    let kernel_dynamic = main_dynamic(aux, kernel_load_bias);
    let direct_invocation = aux.base == 0 || kernel_dynamic == rtld_dynamic;
    let rtld_load_bias = if direct_invocation {
        rtld_load_bias_from_dynamic(aux, rtld_dynamic, kernel_load_bias)
    } else {
        aux.base
    };

    let Some(rtld_dynamic_info) = (unsafe { DynamicInfo::parse(rtld_dynamic) }) else {
        exit(127);
    };
    if !unsafe { relocate_rtld_relative(rtld_dynamic_info, rtld_load_bias) } {
        exit(127);
    }
    unsafe {
        addr_of_mut!(_dl_argv).write(argv);
        addr_of_mut!(__libc_stack_end).write(stack);
    }

    if direct_invocation {
        let direct = unsafe { handle_direct_invocation(argc, argv) };
        let rewritten = unsafe { rewrite_initial_stack_for_program(stack, argc, direct) };
        let state = unsafe {
            publish_bootstrap_objects(
                rewritten.argc,
                rewritten.argv,
                rewritten.envp,
                rewritten.auxv,
                aux,
                0,
                null(),
                rtld_load_bias,
                rtld_dynamic,
                BootstrapMode::DirectExec,
                rewritten.exec_path,
            )
        };
        match unsafe { rtld_stage1(&state) } {
            Ok(entry) => return entry,
            Err(err) => {
                write_stage1_error(b"rtld: direct exec failed: ", &err);
                exit(127);
            }
        }
    }

    let main_load_bias = kernel_load_bias;
    let main_dynamic = kernel_dynamic;
    let state = unsafe {
        publish_bootstrap_objects(
            argc,
            argv,
            envp_start as *const *const u8,
            auxv,
            aux,
            main_load_bias,
            main_dynamic,
            rtld_load_bias,
            rtld_dynamic,
            BootstrapMode::KernelMappedMain,
            null(),
        )
    };

    let stage1_error = match unsafe { rtld_stage1(&state) } {
        Ok(entry) => return entry,
        Err(err) => err,
    };

    if aux.entry != 0 {
        let can_tail_jump = unsafe { DynamicInfo::parse(main_dynamic) }
            .map(can_tail_jump_main)
            .unwrap_or(false);
        if can_tail_jump {
            return aux.entry;
        }
    }

    write_stage1_error(b"rtld: stage-1 failed: ", &stage1_error);
    exit(127)
}

struct StderrWriter;

impl Write for StderrWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_stderr(s.as_bytes());
        Ok(())
    }
}

fn write_stage1_error(prefix: &[u8], err: &dlopen_rs::Error) {
    write_stderr(prefix);
    let _ = write!(StderrWriter, "{err}");
    write_stderr(b"\n");
}

struct RewrittenStack {
    argc: usize,
    argv: *const *const u8,
    envp: *const *const u8,
    auxv: *const usize,
    exec_path: *const u8,
}

unsafe fn rewrite_initial_stack_for_program(
    stack: *const usize,
    argc: usize,
    direct: DirectProgram,
) -> RewrittenStack {
    let new_argc = argc.wrapping_sub(direct.argv_index);
    let src = unsafe { stack.add(1 + direct.argv_index) };
    let dst = unsafe { stack.add(1) as *mut usize };
    let old_envp = unsafe { stack.add(argc.wrapping_add(2)) };
    let mut old_auxv = old_envp;
    while unsafe { read_usize(old_auxv) } != 0 {
        old_auxv = unsafe { old_auxv.add(1) };
    }
    old_auxv = unsafe { old_auxv.add(1) };
    let mut old_end = old_auxv;
    while unsafe { read_usize(old_end) } != AT_NULL {
        old_end = unsafe { old_end.add(2) };
    }
    old_end = unsafe { old_end.add(2) };

    let count = (old_end as usize - src as usize) / core::mem::size_of::<usize>();
    let mut index = 0usize;
    while index < count {
        let value = unsafe { read_usize(src.add(index)) };
        unsafe { dst.add(index).write(value) };
        index = index.wrapping_add(1);
    }

    unsafe { (stack as *mut usize).write(new_argc) };
    let exec_path = unsafe { read_usize(dst) as *const u8 };
    if !direct.argv0.is_null() {
        unsafe { dst.write(direct.argv0 as usize) };
    }

    let argv = unsafe { stack.add(1) as *const *const u8 };
    let envp = unsafe { stack.add(new_argc.wrapping_add(2)) as *const *const u8 };
    let mut auxv = envp as *const usize;
    while unsafe { read_usize(auxv) } != 0 {
        auxv = unsafe { auxv.add(1) };
    }
    auxv = unsafe { auxv.add(1) };

    RewrittenStack {
        argc: new_argc,
        argv,
        envp,
        auxv,
        exec_path,
    }
}

unsafe fn parse_auxv(mut auxp: *const usize) -> AuxState {
    let mut aux = AuxState::empty();
    loop {
        let kind = unsafe { read_usize(auxp) };
        let value = unsafe { read_usize(auxp.add(1)) };
        auxp = unsafe { auxp.add(2) };
        match kind {
            AT_NULL => return aux,
            AT_PHDR => aux.phdr = value,
            AT_PHENT => aux.phent = value,
            AT_PHNUM => aux.phnum = value,
            AT_BASE => aux.base = value,
            AT_ENTRY => aux.entry = value,
            AT_SECURE => aux.secure = value,
            AT_PAGESZ => aux.pagesize = value,
            AT_PLATFORM => aux.platform = value,
            AT_HWCAP => aux.hwcap = value,
            AT_HWCAP2 => aux.hwcap2 = value,
            AT_HWCAP3 => aux.hwcap3 = value,
            AT_HWCAP4 => aux.hwcap4 = value,
            AT_CLKTCK => aux.clktck = value,
            AT_FPUCW => aux.fpucw = value,
            AT_MINSIGSTKSZ => aux.minsigstacksize = value,
            AT_SYSINFO_EHDR => aux.sysinfo_ehdr = value,
            _ => {}
        }
    }
}

fn main_load_bias(aux: AuxState) -> usize {
    aux.find_phdr(ElfProgramType::PHDR)
        .map(|phdr| aux.phdr.wrapping_sub(phdr.p_vaddr()))
        .unwrap_or(0)
}

fn main_dynamic(aux: AuxState, load_bias: usize) -> *const usize {
    aux.find_phdr(ElfProgramType::DYNAMIC)
        .map(|phdr| load_bias.wrapping_add(phdr.p_vaddr()) as *const usize)
        .unwrap_or(null())
}

fn rtld_load_bias_from_dynamic(
    aux: AuxState,
    rtld_dynamic: *const usize,
    fallback: usize,
) -> usize {
    if rtld_dynamic.is_null() {
        return fallback;
    }

    aux.find_phdr(ElfProgramType::DYNAMIC)
        .map(|phdr| (rtld_dynamic as usize).wrapping_sub(phdr.p_vaddr()))
        .unwrap_or(fallback)
}

#[derive(Copy, Clone)]
struct DynamicInfo {
    has_needed: bool,
    relocations: RelocationTable,
    relr: usize,
    relrsz: usize,
    relrent: usize,
    jmprel: usize,
    pltrelsz: usize,
}

#[derive(Copy, Clone)]
struct RelocationTable {
    offset: usize,
    size: usize,
    entry_size: usize,
}

impl RelocationTable {
    const fn empty() -> Self {
        Self {
            offset: 0,
            size: 0,
            entry_size: core::mem::size_of::<ElfRelType>(),
        }
    }

    fn is_empty(self) -> bool {
        self.size == 0
    }
}

impl DynamicInfo {
    const fn empty() -> Self {
        Self {
            has_needed: false,
            relocations: RelocationTable::empty(),
            relr: 0,
            relrsz: 0,
            relrent: core::mem::size_of::<usize>(),
            jmprel: 0,
            pltrelsz: 0,
        }
    }

    fn has_regular_relocations(self) -> bool {
        !self.relocations.is_empty()
    }

    unsafe fn parse(dynamic: *const usize) -> Option<Self> {
        if dynamic.is_null() {
            return Some(Self::empty());
        }
        let dynamic = dynamic.cast::<ElfDyn>();
        let mut info = Self::empty();
        let mut index = 0usize;
        while index < 4096 {
            let entry = unsafe { core::ptr::read_unaligned(dynamic.add(index)) };
            let tag = entry.tag();
            let value = entry.value();
            if tag == ElfDynamicTag::NULL {
                return Some(info);
            } else if tag == ElfDynamicTag::NEEDED {
                info.has_needed = true;
            } else if tag == NATIVE_RELOCATION_TAG {
                info.relocations.offset = value;
            } else if tag == NATIVE_RELOCATION_SIZE_TAG {
                info.relocations.size = value;
            } else if tag == NATIVE_RELOCATION_ENTRY_SIZE_TAG && value != 0 {
                info.relocations.entry_size = value;
            } else if tag == ElfDynamicTag::RELR {
                info.relr = value;
            } else if tag == ElfDynamicTag::RELRSZ {
                info.relrsz = value;
            } else if tag == ElfDynamicTag::RELRENT && value != 0 {
                info.relrent = value;
            } else if tag == ElfDynamicTag::JMPREL {
                info.jmprel = value;
            } else if tag == ElfDynamicTag::PLTRELSZ {
                info.pltrelsz = value;
            }
            index = index.wrapping_add(1);
        }

        None
    }
}

#[cfg(any(target_arch = "x86", target_arch = "arm"))]
const NATIVE_RELOCATION_TAG: ElfDynamicTag = ElfDynamicTag::REL;
#[cfg(all(not(target_arch = "x86"), not(target_arch = "arm")))]
const NATIVE_RELOCATION_TAG: ElfDynamicTag = ElfDynamicTag::RELA;

#[cfg(any(target_arch = "x86", target_arch = "arm"))]
const NATIVE_RELOCATION_SIZE_TAG: ElfDynamicTag = ElfDynamicTag::RELSZ;
#[cfg(all(not(target_arch = "x86"), not(target_arch = "arm")))]
const NATIVE_RELOCATION_SIZE_TAG: ElfDynamicTag = ElfDynamicTag::RELASZ;

#[cfg(any(target_arch = "x86", target_arch = "arm"))]
const NATIVE_RELOCATION_ENTRY_SIZE_TAG: ElfDynamicTag = ElfDynamicTag::RELENT;
#[cfg(all(not(target_arch = "x86"), not(target_arch = "arm")))]
const NATIVE_RELOCATION_ENTRY_SIZE_TAG: ElfDynamicTag = ElfDynamicTag::RELAENT;

unsafe fn relocate_rtld_relative(info: DynamicInfo, load_bias: usize) -> bool {
    if info.has_needed || info.jmprel != 0 || info.pltrelsz != 0 {
        return false;
    }

    unsafe {
        apply_relocations(info.relocations, load_bias) && apply_relr_relocations(info, load_bias)
    }
}

fn can_tail_jump_main(info: DynamicInfo) -> bool {
    !info.has_needed
        && !info.has_regular_relocations()
        && info.relr == 0
        && info.relrsz == 0
        && info.jmprel == 0
        && info.pltrelsz == 0
}

unsafe fn apply_relocations(table: RelocationTable, load_bias: usize) -> bool {
    if table.size == 0 {
        return true;
    }
    if table.offset == 0 || table.entry_size != core::mem::size_of::<ElfRelType>() {
        return false;
    }
    if table.size % table.entry_size != 0 {
        return false;
    }

    let count = table.size / table.entry_size;
    let relocations = unsafe {
        core::slice::from_raw_parts(
            load_bias.wrapping_add(table.offset) as *const ElfRelType,
            count,
        )
    };
    for rel in relocations {
        match rel.r_type() {
            relocation_type if relocation_type.is_none() => {}
            relocation_type if relocation_type.is_relative() => {
                let dst = load_bias.wrapping_add(rel.r_offset()) as *mut usize;
                let addend = rel.r_addend(load_bias);
                unsafe { dst.write(add_signed(load_bias, addend)) };
            }
            _ => return false,
        }
    }
    true
}

unsafe fn apply_relr_relocations(info: DynamicInfo, load_bias: usize) -> bool {
    if info.relrsz == 0 {
        return true;
    }
    if info.relr == 0 || info.relrent < core::mem::size_of::<usize>() {
        return false;
    }
    if info.relrsz % info.relrent != 0 {
        return false;
    }

    let count = info.relrsz / info.relrent;
    let mut reloc_addr = null_mut::<usize>();
    let mut index = 0usize;
    while index < count {
        let ptr = load_bias
            .wrapping_add(info.relr)
            .wrapping_add(index.wrapping_mul(info.relrent)) as *const usize;
        let value = unsafe { core::ptr::read_unaligned(ptr) };
        if (value & 1) == 0 {
            reloc_addr = load_bias.wrapping_add(value) as *mut usize;
            unsafe { reloc_addr.write(load_bias.wrapping_add(reloc_addr.read())) };
            reloc_addr = unsafe { reloc_addr.add(1) };
        } else {
            if reloc_addr.is_null() {
                return false;
            }
            let mut bitmap = value >> 1;
            let mut dst = reloc_addr;
            while bitmap != 0 {
                if (bitmap & 1) != 0 {
                    unsafe { dst.write(load_bias.wrapping_add(dst.read())) };
                }
                bitmap >>= 1;
                dst = unsafe { dst.add(1) };
            }
            reloc_addr = unsafe { reloc_addr.add(usize::BITS as usize - 1) };
        }
        index = index.wrapping_add(1);
    }
    true
}

fn add_signed(base: usize, addend: isize) -> usize {
    if addend >= 0 {
        base.wrapping_add(addend as usize)
    } else {
        base.wrapping_sub(addend.wrapping_neg() as usize)
    }
}

struct RtldElfInfo {
    phdr: *const ElfPhdr,
    phnum: usize,
    entry: usize,
}

fn rtld_elf_info(load_bias: usize) -> RtldElfInfo {
    if load_bias == 0 {
        return RtldElfInfo {
            phdr: null(),
            phnum: 0,
            entry: 0,
        };
    }

    let ehdr = unsafe { core::ptr::read_unaligned(load_bias as *const ElfHeader) };
    if ehdr.e_phentsize() < core::mem::size_of::<ElfPhdr>() {
        return RtldElfInfo {
            phdr: null(),
            phnum: 0,
            entry: 0,
        };
    }

    RtldElfInfo {
        phdr: load_bias.wrapping_add(ehdr.e_phoff()) as *const ElfPhdr,
        phnum: ehdr.e_phnum(),
        entry: load_bias.wrapping_add(ehdr.e_entry()),
    }
}

unsafe fn publish_bootstrap_objects(
    argc: usize,
    argv: *const *const u8,
    envp: *const *const u8,
    auxv: *const usize,
    aux: AuxState,
    main_load_bias: usize,
    main_dynamic: *const usize,
    rtld_load_bias: usize,
    rtld_dynamic: *const usize,
    mode: BootstrapMode,
    exec_path: *const u8,
) -> BootstrapState {
    let main = addr_of_mut!(MAIN_LINK_MAP);
    let rtld = unsafe { rtld_link_map() };
    let rtld_info = rtld_elf_info(rtld_load_bias);

    unsafe {
        main.write(LinkMap {
            l_addr: main_load_bias as *mut c_void,
            l_name: EMPTY_NAME.as_ptr().cast(),
            l_ld: main_dynamic as *mut c_void,
            l_next: rtld,
            l_prev: null_mut(),
            l_real: main,
            l_phdr: aux.phdr as *const ElfPhdr,
            l_entry: aux.entry,
            l_phnum: aux.phnum as u16,
            ..LinkMap::zero()
        });
        rtld.write(LinkMap {
            l_addr: rtld_load_bias as *mut c_void,
            l_name: RTLD_NAME.as_ptr().cast(),
            l_ld: rtld_dynamic as *mut c_void,
            l_next: null_mut(),
            l_prev: main,
            l_real: rtld,
            l_phdr: rtld_info.phdr,
            l_entry: rtld_info.entry,
            l_phnum: rtld_info.phnum as u16,
            ..LinkMap::zero()
        });
        let r_debug = RDebug {
            version: 1,
            map: main,
            brk: Some(crate::symbols::_dl_debug_state),
            state: RT_CONSISTENT,
            ldbase: rtld_load_bias as *mut c_void,
        };
        addr_of_mut!(_r_debug).write(r_debug);
        publish_rtld_globals(
            main,
            rtld,
            r_debug,
            RtldGlobalRoAux {
                auxv,
                platform: aux.platform as *const u8,
                pagesize: aux.pagesize,
                minsigstacksize: aux.minsigstacksize,
                clktck: aux.clktck,
                fpucw: aux.fpucw,
                hwcap: aux.hwcap,
                hwcap2: aux.hwcap2,
                hwcap3: aux.hwcap3,
                hwcap4: aux.hwcap4,
                sysinfo_ehdr: aux.sysinfo_ehdr,
            },
        );
        addr_of_mut!(__libc_enable_secure).write(if aux.secure == 0 { 0 } else { 1 });
    }

    BootstrapState {
        argc,
        argv,
        envp,
        auxv,
        mode,
        exec_path,
        main: BootstrapObject {
            load_bias: main_load_bias,
            dynamic: main_dynamic as *mut c_void,
            phdr: aux.phdr as *const ElfPhdr,
            phnum: aux.phnum,
            entry: aux.entry,
        },
        rtld: BootstrapObject {
            load_bias: rtld_load_bias,
            dynamic: rtld_dynamic as *mut c_void,
            phdr: rtld_info.phdr.cast(),
            phnum: rtld_info.phnum,
            entry: rtld_info.entry,
        },
    }
}
