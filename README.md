[![](https://img.shields.io/crates/v/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![](https://img.shields.io/crates/d/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![license](https://img.shields.io/crates/l/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![dlopen-rs on docs.rs](https://docs.rs/dlopen-rs/badge.svg)](https://docs.rs/dlopen-rs)
[![Rust](https://img.shields.io/badge/rust-1.85.0%2B-blue.svg?maxAge=3600)](https://github.com/weizhiao/dlopen_rs)
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


## Troubleshooting: Symbols Not Loaded in GDB
If you're using `dlopen-rs` to load dynamic libraries **without** overriding `LD_PRELOAD`, `gdb` may fail to resolve symbols.  
For more details, see [Issue #10](https://github.com/weizhiao/rust-dlopen/issues/10).
If you are experiencing this issue, try starting `gdb` with the script `scripts/reload_sos_on_r_brk.py`:

```sh
gdb -x scripts/reload_sos_on_r_brk.py /path/to/binary
```
You may also need to install `elftools` via `pip`.


## Advantages
1. Provides support for loading ELF dynamic libraries to #![no_std] targets.
2. Enables easy runtime replacement of symbols in shared libraries with custom implementations.
3. Typically faster than `ld.so` for dynamic library loading and symbol resolution.
4. Offers Rust-friendly interfaces with ergonomic design.
5. Allows repeated loading of the same dynamic library into memory. Using the `CUSTOM_NOT_REGISTER` flag in OpenFlags enables multiple coexisting copies of a library (identical or modified) in memory, facilitating runtime dynamic library `hot-swapping`.

## Feature
| Feature   | Default | Description                                                                                                                                           |
| --------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| std       | Yes     | Enable `std`                                                                                                                                          |
| debug     | No      | Enable this to use gdb/lldb for debugging loaded dynamic libraries. Note that only dynamic libraries loaded using dlopen-rs can be debugged with gdb. |
| mmap      | Yes     | Enable default implementation on platforms with mmap                                                                                                  |  |
| version   | No      | Activate specific versions of symbols for dynamic library loading                                                                                     |
| tls       | Yes     | Enable this to use thread local storage.                                                                                                              |  |
| unwinding | No      | Enable this to use the exception handling mechanism provided by dlopen-rs.                                                                            |
| libgcc    | Yes     | Enable this if the program uses libgcc to handle exceptions.                                                                                          |
| libunwind | No      | Enable this if the program uses libunwind to handle exceptions.                                                                                       |

## Architecture Support

| Arch        | Support | Lazy Binding | Test    |
| ----------- | ------- | ------------ | ------- |
| x86_64      | ✅       | ✅            | ✅(CI)   |
| aarch64     | ✅       | ✅            | ✅(QEMU) |
| riscv64     | ✅       | ✅            | ✅(QEMU) |
| loongarch64 | ✅       | ❌            | ❌       |

## Examples

### Example 1
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

### Example 2
Fine-grained control of the load flow of the dynamic library, you can replace certain functions in the dynamic library that need to be relocated with your own implementation. In the following example, we replace `malloc` with `mymalloc` in the dynamic library and turn on lazy binding.
```rust
use dlopen_rs::ELFLibrary;
use libc::size_t;
use std::{ffi::c_void, path::Path};

extern "C" fn mymalloc(size: size_t) -> *mut c_void {
    println!("malloc:{}bytes", size);
    unsafe { libc::malloc(size) }
}

fn main() {
    std::env::set_var("RUST_LOG", "debug");
    env_logger::init();
    dlopen_rs::init();
    let path = Path::new("./target/release/libexample.so");
    let libc = ElfLibrary::load_existing("libc.so.6").unwrap();
    let libgcc = ElfLibrary::load_existing("libgcc_s.so.1").unwrap();

    let libexample = ElfLibrary::from_file(path, OpenFlags::CUSTOM_NOT_REGISTER)
        .unwrap()
        .relocate_with(&[libc, libgcc], &|name: &str| {
            if name == "malloc" {
                return Some(mymalloc as _);
            } else {
                return None;
            }
        })
        .unwrap();

    let add = unsafe { libexample.get::<fn(i32, i32) -> i32>("add").unwrap() };
    println!("{}", add(1, 1));

    let print = unsafe { libexample.get::<fn(&str)>("print").unwrap() };
    print("dlopen-rs: hello world");
}
```

## Minimum Supported Rust Version
Rust 1.85 or higher.

## TODO
* dlinfo have not been implemented yet. dlerror currently only returns NULL.  
* RTLD_NEXT for dlsym has not been implemented.
* When dlopen fails, the newly loaded dynamic library is destroyed, but the functions in .fini are not called.

## Supplement
If you encounter any issues during use, feel free to raise them on GitHub. We warmly welcome everyone to contribute code to help improve the functionality of dlopen-rs. 😊
