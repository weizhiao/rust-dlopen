//!A Rust library that implements a series of interfaces such as `dlopen` and `dlsym`, consistent with the behavior of libc,
//!providing robust support for dynamic library loading and symbol resolution.
//!
//!This library serves four purposes:
//!1. Provide a pure Rust alternative to musl ld.so or glibc ld.so.
//!2. Provide loading ELF dynamic libraries support for `#![no_std]` targets.
//!3. Easily swap out symbols in shared libraries with your own custom symbols at runtime
//!4. Faster than `ld.so` in most cases (loading dynamic libraries and getting symbols)
//!
//!Currently, it supports `x86_64`, `RV64`, and `AArch64` architectures.
//!
//! # Examples
//! ```no_run
//! # use dlopen_rs::{ElfLibrary, OpenFlags};
//! use std::path::Path;
//!
//! fn main(){
//!     dlopen_rs::init();
//!     let path = Path::new("./target/release/libexample.so");
//!     let libexample = ElfLibrary::dlopen(path, OpenFlags::RTLD_LOCAL | OpenFlags::RTLD_LAZY).unwrap();
//!
//!     let add = unsafe {
//!         libexample.get::<fn(i32, i32) -> i32>("add").unwrap()
//!     };
//!     println!("{}", add(1,1));
//! }
//! ```
#![allow(clippy::type_complexity)]
#![warn(
    clippy::unnecessary_lazy_evaluations,
    clippy::collapsible_if,
    clippy::explicit_iter_loop,
    clippy::manual_assert,
    clippy::needless_question_mark,
    clippy::needless_return,
    clippy::needless_update,
    clippy::redundant_clone,
    clippy::redundant_else,
    clippy::redundant_static_lifetimes
)]
#![no_std]

extern crate alloc;

pub mod abi;
#[cfg(feature = "debug")]
mod debug;
mod dl_iterate_phdr;
mod dladdr;
mod dlopen;
mod dlsym;
mod find;
#[cfg(feature = "use-ldso")]
mod init;
mod loader;
mod register;
#[cfg(feature = "tls")]
mod tls;
use alloc::{
    boxed::Box,
    string::{String, ToString},
};
use bitflags::bitflags;
use core::{any::Any, fmt::Display};

pub use elf_loader::{Symbol, mmap::Mmap};
#[cfg(feature = "use-ldso")]
pub use init::init;
pub use loader::{Builder, ElfLibrary};

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64",
)))]
compile_error!("unsupport arch");

bitflags! {
    /// Control how dynamic libraries are loaded.
    #[derive(Clone, Copy, Debug)]
    pub struct OpenFlags:u32{
        /// This is the converse of RTLD_GLOBAL, and the default if neither flag is specified.
        /// Symbols defined in this shared object are not made available to resolve references in subsequently loaded shared objects.
        const RTLD_LOCAL = 0;
        /// Perform lazy binding. Resolve symbols only as the code that references them is executed.
        /// If the symbol is never referenced, then it is never resolved.
        const RTLD_LAZY = 1;
        /// If this value is specified, or the environment variable LD_BIND_NOW is set to a nonempty string,
        /// all undefined symbols in the shared object are resolved before dlopen() returns.
        const RTLD_NOW= 2;
        /// Not supported
        const RTLD_NOLOAD = 4;
        /// Not supported
        const RTLD_DEEPBIND =8;
        /// The symbols defined by this shared object will be made available for symbol resolution of subsequently loaded shared objects.
        const RTLD_GLOBAL = 256;
        /// Do not unload the shared object during dlclose(). Consequently,
        /// the object's static and global variables are not reinitialized if the object is reloaded with dlopen() at a later time.
        const RTLD_NODELETE = 4096;
        /// dlopen-rs custom flag, true local loading, does not involve any global variable operations, no lock, and has the fastest loading speed.
        const CUSTOM_NOT_REGISTER = 1024;
    }
}

/// dlopen-rs error type
#[derive(Debug)]
pub enum Error {
    /// Returned when encountered a loader error.
    LoaderError { err: elf_loader::Error },
    /// Returned when failed to find a library.
    FindLibError { msg: String },
    /// Returned when failed to find a symbol.
    FindSymbolError { msg: String },
    /// Returned when failed to iterate phdr.
    IteratorPhdrError { err: Box<dyn Any> },
}

impl Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::LoaderError { err } => write!(f, "{err}"),
            Error::FindLibError { msg } => write!(f, "{msg}"),
            Error::FindSymbolError { msg } => write!(f, "{msg}"),
            Error::IteratorPhdrError { err } => write!(f, "{:?}", err),
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
fn find_lib_error(msg: impl ToString) -> Error {
    Error::FindLibError {
        msg: msg.to_string(),
    }
}

#[cold]
#[inline(never)]
fn find_symbol_error(msg: impl ToString) -> Error {
    Error::FindSymbolError {
        msg: msg.to_string(),
    }
}

pub type Result<T> = core::result::Result<T, Error>;
