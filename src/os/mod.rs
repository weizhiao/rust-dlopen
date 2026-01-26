cfg_if::cfg_if! {
    if #[cfg(feature = "use-syscall")] {
        mod linux;
        pub(crate) use linux::*;
    } else if #[cfg(unix)] {
        mod unix_libc;
        pub(crate) use unix_libc::*;
    } else {
        pub(crate) fn read_file(_path: &str) -> crate::Result<alloc::boxed::Box<[u8]>> {
            Err(crate::Error::Unsupported)
        }
    }
}
