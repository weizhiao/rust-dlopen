#![cfg(target_os = "linux")]

use dlopen_rs::{ElfLibrary, OpenFlags};

#[test]
fn glibc_234_compat_sonames_resolve_to_libc() {
    for soname in ["libdl.so.2", "libpthread.so.0", "librt.so.1"] {
        let lib = ElfLibrary::dlopen(soname, OpenFlags::RTLD_NOW).unwrap();
        assert!(
            lib.shortname() == "libc.so.6" || lib.name().contains("libc.so"),
            "{soname} resolved to {} ({}) instead of libc",
            lib.shortname(),
            lib.name()
        );
    }
}
