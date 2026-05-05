use crate::Result;
use crate::core_impl::types::FileIdentity;
use alloc::boxed::Box;

pub(crate) fn read_file(path: &str) -> Result<Box<[u8]>> {
    std::fs::read(path)
        .map(|v| v.into_boxed_slice())
        .map_err(crate::Error::from)
}

pub(crate) fn read_file_limit(path: &str, limit: usize) -> Result<Box<[u8]>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = alloc::vec![0; limit];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf.into_boxed_slice())
}

pub(crate) fn get_file_inode(path: &str) -> Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    Ok(FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}
