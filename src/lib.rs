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
//!
//! fn main(){
//!     dlopen_rs::init();
//!     let path = "./target/release/libexample.so";
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

pub mod api;
mod arch;
mod core_impl;
mod error;
mod os;
mod utils;

use bitflags::bitflags;

pub use crate::core_impl::init::init;
pub use crate::core_impl::loader::ElfLibrary;
pub use crate::error::Error;
pub use elf_loader::image::Symbol;

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64",
)))]
compile_error!("unsupport arch");

bitflags! {
    /// Flags that control how dynamic libraries are loaded and resolved.
    #[derive(Clone, Copy, Debug)]
    pub struct OpenFlags:u32{
        /// Symbols in this library are not made available to resolve references in subsequently loaded libraries.
        const RTLD_LOCAL = 0;
        /// Perform lazy binding: resolve symbols only as they are executed.
        const RTLD_LAZY = 1;
        /// Resolve all symbols before `dlopen` returns.
        const RTLD_NOW = 2;
        /// Not supported.
        const RTLD_NOLOAD = 4;
        /// Prefer the search scope of this library over the global scope for symbol resolution.
        const RTLD_DEEPBIND = 8;
        /// Make symbols in this library available for symbol resolution in subsequently loaded libraries.
        const RTLD_GLOBAL = 256;
        /// Do not unload the library during `dlclose`.
        const RTLD_NODELETE = 4096;
        /// dlopen-rs custom flag: fast local loading that bypasses the global registry and locking.
        const CUSTOM_NOT_REGISTER = 1024;
    }
}

pub type Result<T> = core::result::Result<T, Error>;
