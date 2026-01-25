use alloc::{
    boxed::Box,
    string::{String, ToString},
};
use core::{any::Any, fmt::Display};

/// Errors that can occur during dynamic library loading or symbol resolution.
#[derive(Debug)]
pub enum Error {
    /// An error occurred within the underlying ELF loader.
    LoaderError { err: elf_loader::Error },
    /// Failed to find the specified library.
    FindLibError { msg: String },
    /// Failed to find the specified symbol.
    FindSymbolError { msg: String },
    /// Failed to iterate over program headers.
    IteratorPhdrError { err: Box<dyn Any> },
    /// Failed to parse the `ld.so.cache`.
    ParseLdCacheError { msg: String },
    /// The provided path is invalid.
    InvalidPath,
}

impl Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::LoaderError { err } => write!(f, "{err}"),
            Error::FindLibError { msg } => write!(f, "{msg}"),
            Error::FindSymbolError { msg } => write!(f, "{msg}"),
            Error::IteratorPhdrError { err } => write!(f, "{:?}", err),
            Error::ParseLdCacheError { msg } => write!(f, "{msg}"),
            Error::InvalidPath => write!(f, "Invalid path"),
        }
    }
}

impl From<elf_loader::Error> for Error {
    #[cold]
    fn from(value: elf_loader::Error) -> Self {
        Error::LoaderError { err: value }
    }
}

#[cold]
#[inline(never)]
pub(crate) fn find_lib_error(msg: impl ToString) -> Error {
    Error::FindLibError {
        msg: msg.to_string(),
    }
}

#[cold]
#[inline(never)]
pub(crate) fn find_symbol_error(msg: impl ToString) -> Error {
    Error::FindSymbolError {
        msg: msg.to_string(),
    }
}

#[cold]
#[inline(never)]
pub(crate) fn parse_ld_cache_error(msg: impl ToString) -> Error {
    Error::ParseLdCacheError {
        msg: msg.to_string(),
    }
}
