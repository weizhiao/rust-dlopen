use crate::error::parse_ld_cache_error;
use crate::Result;
use alloc::boxed::Box;
use alloc::{string::String, vec::Vec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LdCacheEntry {
    pub name: String,
    pub path: String,
}

pub(crate) fn build_ld_cache() -> Result<Box<[LdCacheEntry]>> {
    let buffer = crate::os::read_file("/etc/ld.so.cache")?;
    parse_ld_cache(&buffer)
}

fn parse_ld_cache(data: &[u8]) -> Result<Box<[LdCacheEntry]>> {
    const MAGIC_NEW: &[u8] = b"glibc-ld.so.cache1.1";

    // 1. 修复：在整个文件中搜索，而不是只搜前4KB
    // 很多系统的旧缓存区会超过4KB
    let start_offset = data
        .windows(MAGIC_NEW.len())
        .position(|window| window == MAGIC_NEW)
        .ok_or(parse_ld_cache_error(
            "Could not find glibc-ld.so.cache1.1 magic",
        ))?;

    parse_new_format(&data[start_offset..])
}

fn parse_new_format(data: &[u8]) -> Result<Box<[LdCacheEntry]>> {
    // data 现在的 0 偏移处就是 MAGIC_NEW
    let header_size = 48; // 标准 glibc new format header 大小

    // 边界检查
    if data.len() < header_size {
        return Err(parse_ld_cache_error("Cache file too small for header"));
    }

    let nlibs = u32::from_le_bytes(data[20..24].try_into().unwrap()) as usize;
    let len_strings = u32::from_le_bytes(data[24..28].try_into().unwrap()) as usize;

    // 2. 优化：优先尝试标准的 24 字节大小
    let mut entry_size = 24;

    // 启发式检测：尝试探测正确的 entry_size
    // 如果标准24字节解出来的字符串看起来不像路径，再尝试其他对齐
    for &e_size in &[24, 32, 64] {
        // 检查第一个条目 (i=0)
        let entry_start = header_size;
        if entry_start + 12 > data.len() {
            continue;
        }

        let value_offset =
            u32::from_le_bytes(data[entry_start + 8..entry_start + 12].try_into().unwrap())
                as usize;

        // 计算字符串表的位置
        let string_table_start = header_size + nlibs * e_size;
        if string_table_start >= data.len() {
            continue;
        }

        // 尝试读取第一个库的路径
        if value_offset < len_strings {
            let full_str_tab = &data[string_table_start..];
            if let Some(s) = extract_string(full_str_tab, value_offset) {
                // 如果第一个路径以 '/' 开头，我们大概率猜对了大小
                if s.starts_with('/') {
                    entry_size = e_size;
                    break;
                }
            }
        }
    }

    let string_table_offset = header_size + nlibs * entry_size;
    if string_table_offset >= data.len() {
        return Err(parse_ld_cache_error("String table offset out of bounds"));
    }

    // 字符串表区域
    let string_table = &data[string_table_offset..];
    let mut entries = Vec::with_capacity(nlibs);

    for i in 0..nlibs {
        let entry_start = header_size + i * entry_size;
        if entry_start + 12 > data.len() {
            break;
        }

        let key_idx =
            u32::from_le_bytes(data[entry_start + 4..entry_start + 8].try_into().unwrap()) as usize;
        let val_idx =
            u32::from_le_bytes(data[entry_start + 8..entry_start + 12].try_into().unwrap())
                as usize;

        if let (Some(name), Some(path)) = (
            extract_string(string_table, key_idx),
            extract_string(string_table, val_idx),
        ) {
            entries.push(LdCacheEntry { name, path });
        }
    }

    Ok(entries.into_boxed_slice())
}

// 辅助函数保持不变...
fn extract_string(table: &[u8], offset: usize) -> Option<String> {
    if offset >= table.len() {
        return None;
    }
    let slice = &table[offset..];
    let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    core::str::from_utf8(&slice[..len]).ok().map(String::from)
}
