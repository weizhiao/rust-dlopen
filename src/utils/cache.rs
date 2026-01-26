use crate::error::parse_ld_cache_error;
use crate::{Result, api::dlopen::ElfPath};
use alloc::boxed::Box;
use alloc::{string::String, vec::Vec};

pub(crate) fn build_ld_cache() -> Result<Box<[ElfPath]>> {
    // 尝试读取 /etc/ld.so.cache 文件
    let buffer = crate::os::read_file("/etc/ld.so.cache")?;

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
    if data.len() < 48 {
        return Err(parse_ld_cache_error("ld.so.cache file is too small"));
    }

    // 解析头部信息
    // glibc new format header is 48 bytes
    let nlibs = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    let len_strings = u32::from_le_bytes([data[24], data[25], data[26], data[27]]) as usize;

    let header_size = 48;
    // in new format, entries are usually 24 bytes, but some systems use 32
    // we can try to infer it from file size if needed, but 24 is standard for recent glibc
    let entry_size = 24;
    let string_table_offset = header_size + (nlibs as usize) * entry_size;

    // 修正边界检查逻辑，确保字符串表在文件范围内
    if string_table_offset > data.len() {
        // Try fallback to 32 bytes entry size if 24 failed (some older/different glibc)
        let entry_size_alt = 32;
        let string_table_offset_alt = header_size + (nlibs as usize) * entry_size_alt;
        if string_table_offset_alt > data.len() {
            return Err(parse_ld_cache_error(
                "Invalid ld.so.cache format: entries exceed file size",
            ));
        }
        // If we reach here, maybe it's 32? (uncommon for 1.1)
    }

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

    while offset + entry_size <= string_table_offset && entry_count < nlibs as usize {
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

        offset += entry_size; // 下一个条目
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
