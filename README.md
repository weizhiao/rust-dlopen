# dlopen-rs

<p align="center">
  <a href="https://crates.io/crates/dlopen-rs"><img src="https://img.shields.io/crates/v/dlopen-rs.svg" alt="Crates.io"></a>
  <a href="https://crates.io/crates/dlopen-rs"><img src="https://img.shields.io/crates/d/dlopen-rs.svg" alt="Downloads"></a>
  <a href="https://docs.rs/dlopen-rs/"><img src="https://docs.rs/dlopen-rs/badge.svg" alt="Docs.rs"></a>
  <a href="https://github.com/weizhiao/dlopen-rs/actions"><img src="https://github.com/weizhiao/dlopen-rs/actions/workflows/rust.yml/badge.svg" alt="Build Status"></a>
  <img src="https://img.shields.io/badge/rust-1.93.0%2B-blue.svg" alt="MSRV">
  <img src="https://img.shields.io/crates/l/dlopen-rs.svg" alt="License">
</p>

<p align="center">
  <b>A high-performance, pure-Rust implementation of an ELF dynamic linker.</b>
</p>

<p align="center">
  <a href="README.md">English</a> | <a href="README-zh_cn.md">简体中文</a>
</p>

`dlopen-rs` is a full-featured dynamic linker implemented entirely in Rust. It provides a set of Rust-friendly interfaces for manipulating dynamic libraries, as well as C-compatible interfaces consistent with `libc` behavior.

## 🚀 Key Features

- **Pure Rust:** Zero dependency on C runtime for core loading and linking logic.
- **`#![no_std]` Support:** Can be used in bare-metal environments, kernels, or embedded systems.
- **`LD_PRELOAD` Compatible:** Can replace `libc`'s `dlopen`, `dlsym`, and `dl_iterate_phdr` without code modification.
- **Modern API:** Offers a safe and ergonomic Rust API for dynamic library management.

## 🛠 Usage

### As a Rust Library
Add `dlopen-rs` to your `Cargo.toml`:
```toml
[dependencies]
dlopen-rs = "0.8.0"
```

### As an `LD_PRELOAD` Replacement

You can use `dlopen-rs` to intercept standard library calls:

```shell
# 1. Compile the compatibility library
$ cargo build -r -p cdylib

# 2. Compile your application/example
$ cargo build -r -p dlopen-rs --example preload

# 3. Interpose libc implementations
$ RUST_LOG=trace LD_PRELOAD=./target/release/libdlopen.so ./target/release/examples/preload
```

## 📊 Architecture Support

| Architecture | Load Support | Lazy Binding | Test Status |
| ------------ | ------------ | ------------ | ----------- |
| **x86_64**   | ✅            | ✅            | ✅ (CI)      |
| **aarch64**  | ✅            | ✅            | 🛠️ (Manual)  |
| **riscv64**  | ✅            | ✅            | 🛠️ (Manual)  |

## 💻 Example

```rust
use dlopen_rs::{ElfLibrary, OpenFlags, Result};

fn main() -> Result<()> {
    // 1. Load the library
    let lib = ElfLibrary::dlopen("./target/release/libexample.so", OpenFlags::RTLD_LAZY)?;

    // 2. Get and call a simple function: fn(i32, i32) -> i32
    let add = unsafe { lib.get::<fn(i32, i32) -> i32>("add")? };
    println!("add(1, 1) = {}", add(1, 1));

    Ok(())
}
```

## ⚙️ Feature Flags

| Feature       | Default | Description                                                              |
| ------------- | ------- | ------------------------------------------------------------------------ |
| `use-syscall` | ❌       | Uses direct syscalls to load libraries (useful for `no_std`).            |
| `version`     | ❌       | Enables support for ELF symbol versioning.                               |
| `std`         | ✅       | Enables standard library integration. Disable for `no_std` environments. |

## ⚖️ License

Licensed under the [Apache License 2.0](https://www.google.com/search?q=LICENSE).

## 🤝 Contribution

Contributions are welcome! If you encounter issues or have ideas for performance improvements, please open an Issue or PR. For specific GDB-related debugging issues, please refer to [TroubleshootingGdb.md](TroubleshootingGdb.md).

<a href="https://github.com/weizhiao/rust-dlopen/graphs/contributors">
  <img src="https://contributors-img.web.app/image?repo=weizhiao/rust-dlopen" alt="Project Contributors" />
</a>

---

**Minimum Supported Rust Version (MSRV):** 1.93.0 or higher.