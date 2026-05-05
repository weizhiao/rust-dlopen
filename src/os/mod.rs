cfg_if::cfg_if! {
    if #[cfg(feature = "use-syscall")] {
        mod linux;
        pub(crate) use linux::*;
    } else if #[cfg(all(unix, feature = "std"))] {
        mod unix;
        pub(crate) use unix::*;
    } else {
        use crate::core_impl::types::FileIdentity;

        pub(crate) fn read_file(_path: &str) -> crate::Result<alloc::boxed::Box<[u8]>> {
            Err(crate::Error::Unsupported)
        }
        pub(crate) fn read_file_limit(_path: &str, _limit: usize) -> crate::Result<alloc::boxed::Box<[u8]>> {
            Err(crate::Error::Unsupported)
        }
        pub(crate) fn get_file_inode(_path: &str) -> crate::Result<FileIdentity> {
            Err(crate::Error::Unsupported)
        }
    }
}
