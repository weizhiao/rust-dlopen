use crate::{Error, parse_ld_cache_error};
use crate::{Result, dlopen::ElfPath};
use alloc::boxed::Box;
use alloc::{string::String, vec::Vec};

impl From<syscalls::Errno> for Error {
    fn from(value: syscalls::Errno) -> Self {
        parse_ld_cache_error(value)
    }
}

pub(super) fn build_ld_cache() -> Result<Box<[ElfPath]>> {
    // 尝试读取 /etc/ld.so.cache 文件
    let path = b"/etc/ld.so.cache\0"; // C字符串需要以null结尾

    const O_RDONLY: usize = 0;
    // 使用系统调用打开文件
    let fd =
        unsafe { syscalls::syscall2(syscalls::Sysno::open, path.as_ptr() as usize, O_RDONLY)? };

    const SEEK_END: usize = 2;
    const SEEK_SET: usize = 0;
    // 获取文件大小
    let file_size =
        unsafe { syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_END)? };

    // 重置文件指针到开头
    unsafe { syscalls::syscall3(syscalls::Sysno::lseek, fd as usize, 0, SEEK_SET)? };

    // 分配内存并读取文件内容
    let mut buffer = Vec::with_capacity(file_size);
    unsafe {
        buffer.set_len(file_size);
    }

    let bytes_read = unsafe {
        syscalls::syscall3(
            syscalls::Sysno::read,
            fd as usize,
            buffer.as_mut_ptr() as usize,
            file_size,
        )?
    };

    // 关闭文件
    unsafe { syscalls::syscall1(syscalls::Sysno::close, fd as usize)? };

    if bytes_read != file_size {
        return Err(parse_ld_cache_error(
            "Failed to read complete ld.so.cache file",
        ));
    }

    // 解析 ld.so.cache 内容
    parse_ld_cache(&buffer)
}

fn parse_ld_cache(data: &[u8]) -> Result<Box<[ElfPath]>> {
    // 检查新格式的魔数
    let new_cache_magic = b"glibc-ld.so.cache";
    let new_cache_version = b"1.1";

    if data.len() > 20
        && data[0..new_cache_magic.len()] == *new_cache_magic
        && data[17..20] == *new_cache_version
    {
        parse_new_format(data)
    } else {
        // 不支持的格式或旧格式
        return Err(parse_ld_cache_error("Unsupported ld.so.cache format"));
    }
}

fn parse_new_format(data: &[u8]) -> Result<Box<[ElfPath]>> {
    if data.len() < 32 {
        return Err(parse_ld_cache_error("ld.so.cache file is too small"));
    }

    // 解析头部信息
    let header_size = 32;
    let nlibs = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    let len_strings = u32::from_le_bytes([data[24], data[25], data[26], data[27]]) as usize;
    let _flags = data[28];
    let _extension_offset = u32::from_le_bytes([data[29], data[30], data[31], data[32]]) as usize;

    // 计算字符串表的偏移量
    let string_table_offset = header_size + (nlibs as usize) * 32; // 每个条目32字节

    // 修正边界检查逻辑，确保字符串表在文件范围内
    if string_table_offset > data.len() {
        return Err(parse_ld_cache_error("Invalid ld.so.cache format: entries exceed file size"));
    }
    
    // 如果字符串表长度为0，或者超出文件范围，则使用从string_table_offset到文件末尾的所有数据
    let string_table_end = if len_strings == 0 || string_table_offset + len_strings > data.len() {
        data.len()
    } else {
        string_table_offset + len_strings
    };

    // 提取字符串表
    let string_table = &data[string_table_offset..string_table_end];

    // 解析条目并提取路径
    let mut paths = Vec::new();
    let mut offset = header_size;
    let mut entry_count = 0;

    while offset + 32 <= string_table_offset && entry_count < nlibs as usize {
        // 解析条目中的value字段（库文件路径的偏移量）
        if offset + 16 > data.len() {
            break;
        }

        let value_offset = u32::from_le_bytes([
            data[offset + 8],
            data[offset + 9],
            data[offset + 10],
            data[offset + 11],
        ]) as usize;

        // 从字符串表中提取路径
        if value_offset < string_table.len() {
            if let Some(path_str) = extract_string(string_table, value_offset) {
                // 获取目录部分
                if let Some(parent_path) = get_parent_path(&path_str) {
                    if let Ok(elf_path) = ElfPath::from_str(&parent_path) {
                        // 避免重复添加相同的路径
                        if !paths.contains(&elf_path) {
                            paths.push(elf_path);
                        }
                    }
                }
            }
        }

        offset += 32; // 下一个条目
        entry_count += 1;
    }

    Ok(paths.into_boxed_slice())
}

fn extract_string(data: &[u8], offset: usize) -> Option<String> {
    if offset >= data.len() {
        return None;
    }

    let mut end = offset;
    while end < data.len() && data[end] != 0 {
        end += 1;
    }

    if end > offset {
        match core::str::from_utf8(&data[offset..end]) {
            Ok(s) => Some(String::from(s)),
            Err(_) => None,
        }
    } else {
        None
    }
}

fn get_parent_path(path: &str) -> Option<String> {
    // 查找最后一个 '/' 字符
    if let Some(last_slash) = path.rfind('/') {
        if last_slash > 0 {
            Some(String::from(&path[..last_slash]))
        } else {
            Some(String::from("/"))
        }
    } else {
        None
    }
}