[![](https://img.shields.io/crates/v/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![](https://img.shields.io/crates/d/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![license](https://img.shields.io/crates/l/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![dlopen-rs on docs.rs](https://docs.rs/dlopen-rs/badge.svg)](https://docs.rs/dlopen-rs)
[![Rust](https://img.shields.io/badge/rust-1.88.0%2B-blue.svg?maxAge=3600)](https://github.com/weizhiao/dlopen_rs)
[![Build Status](https://github.com/weizhiao/dlopen-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/weizhiao/dlopen-rs/actions)
# dlopen-rs

English | [中文](README-zh_cn.md)  

[[Documentation]](https://docs.rs/dlopen-rs/)

`dlopen-rs` is a dynamic linker fully implemented in Rust, providing a set of Rust-friendly interfaces for manipulating dynamic libraries, as well as C-compatible interfaces consistent with `libc` behavior.

## Usage
You can use `dlopen-rs` as a replacement for `libloading` to load dynamic libraries. It also allows replacing libc's `dlopen`, `dlsym`, `dl_iterate_phdr` and other functions with implementations from `dlopen-rs` using `LD_PRELOAD` without code modifications.
```shell
# Compile the library as a dynamic library
$ cargo build -r -p cdylib
# Compile test cases
$ cargo build -r -p dlopen-rs --example preload
# Replace libc implementations with ours
$ RUST_LOG=trace LD_PRELOAD=./target/release/libdlopen.so ./target/release/examples/preload
```

## Advantages
1. Provides support for loading ELF dynamic libraries to #![no_std] targets.
2. Enables easy runtime replacement of symbols in shared libraries with custom implementations.
3. Typically faster than `ld.so` for dynamic library loading and symbol resolution.
4. Offers Rust-friendly interfaces with ergonomic design.

## Feature
| Feature     | Default | Description                                                       |
| ----------- | ------- | ----------------------------------------------------------------- |
| use-syscall | No      | Use syscalls to load dynamic libraries from the file system.      |
| version     | No      | Activate specific versions of symbols for dynamic library loading |

## Architecture Support

| Arch    | Support | Lazy Binding | Test      |
| ------- | ------- | ------------ | --------- |
| x86_64  | ✅       | ✅            | ✅(CI)     |
| aarch64 | ✅       | ✅            | ✅(Manual) |
| riscv64 | ✅       | ✅            | ✅(Manual) |

## Examples

The `dlopen` interface is used to load dynamic libraries, and the `dl_iterate_phdr` interface is used to iterate through the already loaded dynamic libraries. Additionally, this library uses the `log` crate, and you can use your preferred library to output log information to view the workflow of `dlopen-rs`. In the examples of this library, the `env_logger` crate is used.
```rust
use dlopen_rs::ELFLibrary;
use std::path::Path;

fn main() {
    std::env::set_var("RUST_LOG", "trace");
    env_logger::init();
    dlopen_rs::init();
    let path = Path::new("./target/release/libexample.so");
    let libexample =
        ElfLibrary::dlopen(path, OpenFlags::RTLD_LOCAL | OpenFlags::RTLD_LAZY).unwrap();
    let add = unsafe { libexample.get::<fn(i32, i32) -> i32>("add").unwrap() };
    println!("{}", add(1, 1));

    let print = unsafe { libexample.get::<fn(&str)>("print").unwrap() };
    print("dlopen-rs: hello world");
	
    let dl_info = ElfLibrary::dladdr(print.into_raw() as usize).unwrap();
    println!("{:?}", dl_info);

    ElfLibrary::dl_iterate_phdr(|info| {
        println!(
            "iterate dynamic library: {}",
            unsafe { CStr::from_ptr(info.dlpi_name).to_str().unwrap() }
        );
        Ok(())
    })
    .unwrap();
}
```

## Minimum Supported Rust Version
Rust 1.88 or higher.

## TODO
* dlinfo have not been implemented yet. dlerror currently only returns NULL.  
* RTLD_NEXT for dlsym has not been implemented.
* When dlopen fails, the newly loaded dynamic library is destroyed, but the functions in .fini are not called.
* Fix multi-threading bugs.

## Supplement
If you encounter any issues during use, feel free to raise them on GitHub. We warmly welcome everyone to contribute code to help improve the functionality of dlopen-rs. 😊

## Troubleshooting GDB
[See the dedicated page.](TroubleshootingGdb.md)
