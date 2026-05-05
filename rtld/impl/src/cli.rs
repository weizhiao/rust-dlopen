use core::slice;

use dlopen_rs::rtld::elf::{ElfDynamicTag, ElfProgramType};
use syscalls::Sysno;

use crate::runtime::{exit, write_stderr, write_stdout};

const AT_FDCWD: usize = (-100isize) as usize;
const O_RDONLY: usize = 0;

const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;

const MAX_PHDRS: usize = 256;
const MAX_LOADS: usize = 16;
const MAX_NEEDED: usize = 64;
const MAX_DYN: usize = 4096;

#[derive(Copy, Clone)]
struct LoadSegment {
    vaddr: usize,
    offset: usize,
    filesz: usize,
}

impl LoadSegment {
    const fn empty() -> Self {
        Self {
            vaddr: 0,
            offset: 0,
            filesz: 0,
        }
    }
}

struct ElfInfo {
    needed: [usize; MAX_NEEDED],
    needed_len: usize,
    strtab_offset: usize,
    strtab_size: usize,
}

enum InspectError {
    Open,
    Read,
    NotElf,
    Unsupported,
    NotDynamic,
    BadDynamic,
}

pub(crate) struct DirectProgram {
    pub(crate) argv_index: usize,
    pub(crate) argv0: *const u8,
}

pub(crate) unsafe fn handle_direct_invocation(
    argc: usize,
    argv: *const *const u8,
) -> DirectProgram {
    if argc <= 1 {
        let program = unsafe { argv.read() };
        print_help(program);
        exit(0);
    }

    let mut index = 1usize;
    let mut argv0 = null();
    while index < argc {
        let arg = unsafe { argv.add(index).read() };
        if cstr_eq(arg, b"--help") {
            let program = unsafe { argv.read() };
            print_help(program);
            exit(0);
        }
        if cstr_eq(arg, b"--version") {
            print_version();
            exit(0);
        }
        if cstr_eq(arg, b"--list-diagnostics") {
            print_diagnostics(argc);
            exit(0);
        }
        if cstr_eq(arg, b"--list-tunables") {
            write_stdout(b"dlopen.rtld.tunables: none\n");
            exit(0);
        }
        if cstr_eq(arg, b"--verify") {
            let Some(target) = (unsafe { option_value(argc, argv, index) }) else {
                missing_option_value(arg);
            };
            exit(if verify(target) { 0 } else { 1 });
        }
        if cstr_eq(arg, b"--list") {
            let Some(target) = (unsafe { option_value(argc, argv, index) }) else {
                missing_option_value(arg);
            };
            exit(if list(target) { 0 } else { 1 });
        }
        if cstr_eq(arg, b"--argv0") {
            let Some(value) = (unsafe { option_value(argc, argv, index) }) else {
                missing_option_value(arg);
            };
            argv0 = value;
            index = index.wrapping_add(2);
            continue;
        }

        if consumes_value(arg) {
            if unsafe { option_value(argc, argv, index) }.is_none() {
                missing_option_value(arg);
            }
            index = index.wrapping_add(2);
            continue;
        }
        if cstr_eq(arg, b"--inhibit-cache") {
            index = index.wrapping_add(1);
            continue;
        }
        if starts_with_dash(arg) {
            write_stderr(b"rtld: unrecognized option: ");
            write_cstr_stderr(arg);
            write_stderr(b"\nTry '--help' for more information.\n");
            exit(1);
        }

        return DirectProgram {
            argv_index: index,
            argv0,
        };
    }

    let program = unsafe { argv.read() };
    print_help(program);
    exit(0);
}

unsafe fn option_value(argc: usize, argv: *const *const u8, index: usize) -> Option<*const u8> {
    if index + 1 >= argc {
        None
    } else {
        Some(unsafe { argv.add(index + 1).read() })
    }
}

fn consumes_value(arg: *const u8) -> bool {
    cstr_eq(arg, b"--library-path")
        || cstr_eq(arg, b"--preload")
        || cstr_eq(arg, b"--audit")
        || cstr_eq(arg, b"--glibc-hwcaps-prepend")
        || cstr_eq(arg, b"--glibc-hwcaps-mask")
        || cstr_eq(arg, b"--inhibit-rpath")
}

fn null<T>() -> *const T {
    core::ptr::null()
}

fn missing_option_value(arg: *const u8) -> ! {
    write_stderr(b"rtld: option requires an argument: ");
    write_cstr_stderr(arg);
    write_stderr(b"\n");
    exit(1)
}

fn print_help(program: *const u8) {
    write_stdout(b"Usage: ");
    write_cstr_stdout(program);
    write_stdout(b" [OPTION]... EXECUTABLE-FILE [ARGS-FOR-PROGRAM...]\n");
    write_stdout(b"Rust replacement program interpreter for dynamically linked ELF programs.\n\n");
    write_stdout(b"  --list                list direct DT_NEEDED dependencies\n");
    write_stdout(
        b"  --verify              verify that FILE is a dynamic ELF object this rtld can inspect\n",
    );
    write_stdout(b"  --inhibit-cache       accepted for glibc ld.so command-line compatibility\n");
    write_stdout(
        b"  --library-path PATH   accepted for compatibility; full search override is pending\n",
    );
    write_stdout(
        b"  --preload LIST        accepted for compatibility; preload loading is pending\n",
    );
    write_stdout(b"  --audit LIST          accepted for compatibility; auditing is pending\n");
    write_stdout(
        b"  --argv0 STRING        accepted for compatibility; direct execution is pending\n",
    );
    write_stdout(b"  --glibc-hwcaps-prepend LIST\n");
    write_stdout(b"                        accepted for command-line compatibility\n");
    write_stdout(b"  --glibc-hwcaps-mask LIST\n");
    write_stdout(b"                        accepted for command-line compatibility\n");
    write_stdout(b"  --inhibit-rpath LIST  accepted for command-line compatibility\n");
    write_stdout(b"  --list-tunables       list supported rtld tunables\n");
    write_stdout(b"  --list-diagnostics    list rtld diagnostics\n");
    write_stdout(b"  --help                display this help and exit\n");
    write_stdout(b"  --version             output version information and exit\n\n");
    write_stdout(b"This program interpreter self-identifies as: ld-linux-x86-64.so.2\n");
}

fn print_version() {
    write_stdout(b"dlopen-rs rtld 0.1.0\n");
    write_stdout(b"Command-line compatible subset of glibc ld.so for x86_64 Linux.\n");
}

fn print_diagnostics(argc: usize) {
    write_stdout(b"dlopen.rtld.name=\"ld-linux-x86-64.so.2\"\n");
    write_stdout(b"dlopen.rtld.version=\"0.1.0\"\n");
    write_stdout(b"dlopen.rtld.target=\"x86_64-unknown-linux-gnu\"\n");
    write_stdout(b"dlopen.rtld.argc=");
    write_decimal(argc);
    write_stdout(b"\n");
}

fn verify(path: *const u8) -> bool {
    match inspect(path) {
        Ok(_) => true,
        Err(err) => {
            print_inspect_error(path, err);
            false
        }
    }
}

fn list(path: *const u8) -> bool {
    let info = match inspect(path) {
        Ok(info) => info,
        Err(err) => {
            print_inspect_error(path, err);
            return false;
        }
    };

    let Some(fd) = open(path) else {
        print_inspect_error(path, InspectError::Open);
        return false;
    };

    let mut index = 0usize;
    while index < info.needed_len {
        write_stdout(b"\t");
        print_string_from_file(
            fd,
            info.strtab_offset + info.needed[index],
            info.strtab_size,
        );
        write_stdout(b" => not resolved by stage-0 rtld CLI\n");
        index = index.wrapping_add(1);
    }
    write_stdout(b"\tld-linux-x86-64.so.2 => self\n");
    close(fd);
    true
}

fn inspect(path: *const u8) -> Result<ElfInfo, InspectError> {
    let Some(fd) = open(path) else {
        return Err(InspectError::Open);
    };
    let result = inspect_fd(fd);
    close(fd);
    result
}

fn inspect_fd(fd: usize) -> Result<ElfInfo, InspectError> {
    let mut ehdr = [0u8; 64];
    pread_exact(fd, &mut ehdr, 0).ok_or(InspectError::Read)?;

    if &ehdr[0..4] != b"\x7fELF" {
        return Err(InspectError::NotElf);
    }
    if ehdr[EI_CLASS] != ELFCLASS64 || ehdr[EI_DATA] != ELFDATA2LSB {
        return Err(InspectError::Unsupported);
    }
    let e_type = read_u16(&ehdr, 16);
    let e_machine = read_u16(&ehdr, 18);
    if (e_type != ET_EXEC && e_type != ET_DYN) || e_machine != EM_X86_64 {
        return Err(InspectError::Unsupported);
    }

    let phoff = read_u64(&ehdr, 32) as usize;
    let phentsize = read_u16(&ehdr, 54) as usize;
    let phnum = read_u16(&ehdr, 56) as usize;
    if phentsize < 56 || phnum == 0 || phnum > MAX_PHDRS {
        return Err(InspectError::Unsupported);
    }

    let mut loads = [LoadSegment::empty(); MAX_LOADS];
    let mut load_len = 0usize;
    let mut dynamic_offset = 0usize;
    let mut dynamic_size = 0usize;

    let mut phdr = [0u8; 56];
    let mut index = 0usize;
    while index < phnum {
        let offset = phoff + index * phentsize;
        pread_exact(fd, &mut phdr, offset).ok_or(InspectError::Read)?;
        let program_type = ElfProgramType::new(read_u32(&phdr, 0));
        if program_type == ElfProgramType::LOAD && load_len < MAX_LOADS {
            loads[load_len] = LoadSegment {
                offset: read_u64(&phdr, 8) as usize,
                vaddr: read_u64(&phdr, 16) as usize,
                filesz: read_u64(&phdr, 32) as usize,
            };
            load_len = load_len.wrapping_add(1);
        } else if program_type == ElfProgramType::DYNAMIC {
            dynamic_offset = read_u64(&phdr, 8) as usize;
            dynamic_size = read_u64(&phdr, 32) as usize;
        }
        index = index.wrapping_add(1);
    }

    if dynamic_offset == 0 || dynamic_size == 0 {
        return Err(InspectError::NotDynamic);
    }

    let mut needed = [0usize; MAX_NEEDED];
    let mut needed_len = 0usize;
    let mut strtab_vaddr = 0usize;
    let mut strtab_size = 0usize;
    let dynamic_len = (dynamic_size / 16).min(MAX_DYN);
    let mut dyn_entry = [0u8; 16];
    let mut dyn_index = 0usize;
    while dyn_index < dynamic_len {
        let offset = dynamic_offset + dyn_index * 16;
        pread_exact(fd, &mut dyn_entry, offset).ok_or(InspectError::Read)?;
        let tag = ElfDynamicTag::new(read_i64(&dyn_entry, 0));
        let value = read_u64(&dyn_entry, 8) as usize;
        if tag == ElfDynamicTag::NULL {
            break;
        }
        if tag == ElfDynamicTag::NEEDED && needed_len < MAX_NEEDED {
            needed[needed_len] = value;
            needed_len = needed_len.wrapping_add(1);
        } else if tag == ElfDynamicTag::STRTAB {
            strtab_vaddr = value;
        } else if tag == ElfDynamicTag::STRSZ {
            strtab_size = value;
        }
        dyn_index = dyn_index.wrapping_add(1);
    }

    if strtab_vaddr == 0 || strtab_size == 0 {
        return Err(InspectError::BadDynamic);
    }
    let Some(strtab_offset) = vaddr_to_offset(strtab_vaddr, &loads[..load_len]) else {
        return Err(InspectError::BadDynamic);
    };

    Ok(ElfInfo {
        needed,
        needed_len,
        strtab_offset,
        strtab_size,
    })
}

fn vaddr_to_offset(vaddr: usize, loads: &[LoadSegment]) -> Option<usize> {
    let mut index = 0usize;
    while index < loads.len() {
        let segment = loads[index];
        if vaddr >= segment.vaddr {
            let delta = vaddr - segment.vaddr;
            if delta < segment.filesz {
                return Some(segment.offset + delta);
            }
        }
        index = index.wrapping_add(1);
    }
    None
}

fn print_string_from_file(fd: usize, offset: usize, limit: usize) {
    let mut cursor = offset;
    let end = offset + limit;
    let mut byte = [0u8; 1];
    while cursor < end {
        if pread_exact(fd, &mut byte, cursor).is_none() || byte[0] == 0 {
            return;
        }
        write_stdout(&byte);
        cursor = cursor.wrapping_add(1);
    }
}

fn print_inspect_error(path: *const u8, err: InspectError) {
    write_stderr(b"rtld: ");
    write_cstr_stderr(path);
    write_stderr(b": ");
    match err {
        InspectError::Open => write_stderr(b"cannot open file\n"),
        InspectError::Read => write_stderr(b"cannot read ELF metadata\n"),
        InspectError::NotElf => write_stderr(b"not an ELF object\n"),
        InspectError::Unsupported => write_stderr(b"unsupported ELF object\n"),
        InspectError::NotDynamic => write_stderr(b"not a dynamically linked object\n"),
        InspectError::BadDynamic => write_stderr(b"invalid dynamic section\n"),
    }
}

fn open(path: *const u8) -> Option<usize> {
    unsafe { syscalls::syscall4(Sysno::openat, AT_FDCWD, path as usize, O_RDONLY, 0).ok() }
}

fn close(fd: usize) {
    let _ = unsafe { syscalls::syscall1(Sysno::close, fd) };
}

fn pread_exact(fd: usize, mut buf: &mut [u8], mut offset: usize) -> Option<()> {
    while !buf.is_empty() {
        let read = unsafe {
            syscalls::syscall4(
                Sysno::pread64,
                fd,
                buf.as_mut_ptr() as usize,
                buf.len(),
                offset,
            )
            .ok()?
        };
        if read == 0 || read > buf.len() {
            return None;
        }
        let (_, rest) = buf.split_at_mut(read);
        buf = rest;
        offset = offset.wrapping_add(read);
    }
    Some(())
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    read_u64(bytes, offset) as i64
}

fn starts_with_dash(arg: *const u8) -> bool {
    !arg.is_null() && unsafe { arg.read() } == b'-'
}

fn cstr_eq(ptr: *const u8, expected: &[u8]) -> bool {
    if ptr.is_null() {
        return false;
    }
    let mut index = 0usize;
    while index < expected.len() {
        if unsafe { ptr.add(index).read() } != expected[index] {
            return false;
        }
        index = index.wrapping_add(1);
    }
    unsafe { ptr.add(expected.len()).read() == 0 }
}

fn write_cstr_stdout(ptr: *const u8) {
    write_cstr(1, ptr);
}

fn write_cstr_stderr(ptr: *const u8) {
    write_cstr(2, ptr);
}

fn write_cstr(fd: usize, ptr: *const u8) {
    if ptr.is_null() {
        return;
    }
    let len = cstr_len(ptr);
    let bytes = unsafe { slice::from_raw_parts(ptr, len) };
    if fd == 1 {
        write_stdout(bytes);
    } else {
        write_stderr(bytes);
    }
}

fn cstr_len(ptr: *const u8) -> usize {
    let mut len = 0usize;
    while unsafe { ptr.add(len).read() } != 0 {
        len = len.wrapping_add(1);
    }
    len
}

fn write_decimal(mut value: usize) {
    let mut buf = [0u8; 20];
    let mut index = buf.len();
    if value == 0 {
        write_stdout(b"0");
        return;
    }
    while value != 0 {
        index -= 1;
        buf[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    write_stdout(&buf[index..]);
}
