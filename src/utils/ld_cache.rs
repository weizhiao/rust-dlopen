use crate::Result;
use crate::error::parse_ld_cache_error;
use alloc::boxed::Box;
use alloc::string::String;
use core::cmp::Ordering;

pub(crate) struct LdCache {
    data: Box<[u8]>,
    header_offset: usize,
    nlibs: usize,
    entry_size: usize,
    string_table_offset: usize,
}

const FLAG_TYPE_MASK: i32 = 0x00ff;
const FLAG_LIBC6: i32 = 0x0003;

#[cfg(target_arch = "x86_64")]
const FLAG_ARCH_CURRENT: i32 = 0x0100;
#[cfg(target_arch = "aarch64")]
const FLAG_ARCH_CURRENT: i32 = 0x0200;
#[cfg(target_arch = "riscv64")]
const FLAG_ARCH_CURRENT: i32 = 0x0500;
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
const FLAG_ARCH_CURRENT: i32 = 0x0000;

impl LdCache {
    pub fn new() -> Result<Self> {
        let buffer = crate::os::read_file("/etc/ld.so.cache")?;
        const MAGIC_NEW: &[u8] = b"glibc-ld.so.cache1.1";

        let start_offset = buffer
            .windows(MAGIC_NEW.len())
            .position(|window| window == MAGIC_NEW)
            .ok_or(parse_ld_cache_error(
                "Could not find glibc-ld.so.cache1.1 magic",
            ))?;

        let header = &buffer[start_offset..];
        if header.len() < 48 {
            return Err(parse_ld_cache_error("Cache file too small for header"));
        }

        let nlibs = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;

        // Try to detect entry size (standard is 24, but can be 32, 64)
        let mut entry_size = 24;
        for &e_size in &[24, 32, 64] {
            let entry_start = 48; // relative to start_offset
            if entry_start + 12 > header.len() {
                continue;
            }
            let val_idx = u32::from_le_bytes(
                header[entry_start + 8..entry_start + 12]
                    .try_into()
                    .unwrap(),
            ) as usize;
            let string_table_start = 48 + nlibs * e_size;
            if string_table_start + val_idx < header.len() {
                if let Some(s) = extract_str(&header[string_table_start..], val_idx) {
                    if s.starts_with('/') {
                        entry_size = e_size;
                        break;
                    }
                }
            }
        }

        let string_table_offset = start_offset + 48 + nlibs * entry_size;
        let cache = LdCache {
            data: buffer,
            header_offset: start_offset,
            nlibs,
            entry_size,
            string_table_offset,
        };
        if nlibs > 0 {
            log::debug!(
                "LdCache: first=[{:?}], last=[{:?}]",
                cache.get_name(0),
                cache.get_name(nlibs - 1)
            );
        }
        Ok(cache)
    }

    pub fn lookup(&self, lib_name: &str) -> Option<String> {
        if self.nlibs == 0 {
            return None;
        }

        // Detect if the cache is sorted ascending or descending.
        // glibc usually sorts ascending, but some environments or locales might differ.
        let first_name = self.get_name(0)?;
        let last_name = self.get_name(self.nlibs - 1)?;
        let is_descending = first_name > last_name;
        log::debug!(
            "LD_CACHE lookup: {}, descending={}",
            lib_name,
            is_descending
        );

        let mut left = 0;
        let mut right = self.nlibs;

        while left < right {
            let mid = left + (right - left) / 2;
            let name = self.get_name(mid)?;
            log::trace!("LD_CACHE probe mid={}: {:?}", mid, name);

            let cmp = if is_descending {
                name.cmp(lib_name)
            } else {
                lib_name.cmp(name)
            };

            match cmp {
                Ordering::Equal => {
                    log::debug!("LD_CACHE found name match at index {}", mid);
                    // Search backward to find the first entry with the same name
                    let mut start = mid;
                    while start > 0 && self.get_name(start - 1) == Some(name) {
                        start -= 1;
                    }

                    // Scan forward from the first match
                    let mut i = start;
                    while i < self.nlibs && self.get_name(i) == Some(name) {
                        if self.check_flags(i) {
                            if let Some(path) = self.get_path(i) {
                                log::debug!("LD_CACHE match: {} -> {}", lib_name, path);
                                return Some(path);
                            }
                        }
                        i += 1;
                    }
                    log::debug!("LD_CACHE no flag match for {}", lib_name);
                    return None;
                }
                Ordering::Less => right = mid,
                Ordering::Greater => left = mid + 1,
            }
        }
        log::debug!("LD_CACHE not found: {}", lib_name);
        None
    }

    fn get_name(&self, idx: usize) -> Option<&str> {
        let entry_offset = self.header_offset + 48 + idx * self.entry_size;
        if entry_offset + 8 > self.data.len() {
            return None;
        }
        let key_idx = u32::from_le_bytes(
            self.data[entry_offset + 4..entry_offset + 8]
                .try_into()
                .unwrap(),
        ) as usize;

        // Try relative to header first, then relative to string table
        if let Some(s) = extract_str(&self.data[self.header_offset..], key_idx) {
            if !s.is_empty() && !s.starts_with('/') {
                return Some(s);
            }
        }
        extract_str(&self.data[self.string_table_offset..], key_idx)
    }

    fn get_path(&self, idx: usize) -> Option<String> {
        let entry_offset = self.header_offset + 48 + idx * self.entry_size;
        if entry_offset + 12 > self.data.len() {
            return None;
        }
        let val_idx = u32::from_le_bytes(
            self.data[entry_offset + 8..entry_offset + 12]
                .try_into()
                .unwrap(),
        ) as usize;

        if let Some(s) = extract_str(&self.data[self.header_offset..], val_idx) {
            if s.starts_with('/') {
                return Some(String::from(s));
            }
        }
        extract_str(&self.data[self.string_table_offset..], val_idx).map(String::from)
    }

    fn check_flags(&self, idx: usize) -> bool {
        let entry_offset = self.header_offset + 48 + idx * self.entry_size;
        if entry_offset + 4 > self.data.len() {
            return false;
        }
        let flags = i32::from_le_bytes(
            self.data[entry_offset..entry_offset + 4]
                .try_into()
                .unwrap(),
        ) as u32;

        // Basic arch and type check
        if (flags & FLAG_TYPE_MASK as u32) != FLAG_LIBC6 as u32 {
            return false;
        }

        if FLAG_ARCH_CURRENT != 0 && (flags & FLAG_ARCH_CURRENT as u32) == 0 {
            return false;
        }

        true
    }
}

fn extract_str(table: &[u8], offset: usize) -> Option<&str> {
    if offset >= table.len() {
        return None;
    }
    let slice = &table[offset..];
    let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    core::str::from_utf8(&slice[..len]).ok()
}
