#![no_std]

mod arch;
mod bootstrap;
mod cli;
mod glibc;
mod globals;
mod memory;
mod runtime;
mod symbols;
mod versions;

#[doc(hidden)]
pub fn force_link() {}
