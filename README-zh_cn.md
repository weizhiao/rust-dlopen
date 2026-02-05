[![](https://img.shields.io/crates/v/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![](https://img.shields.io/crates/d/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![license](https://img.shields.io/crates/l/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![dlopen-rs on docs.rs](https://docs.rs/dlopen-rs/badge.svg)](https://docs.rs/dlopen-rs)
[![Rust](https://img.shields.io/badge/rust-1.93.0%2B-blue.svg?maxAge=3600)](https://github.com/weizhiao/dlopen_rs)
[![Build Status](https://github.com/weizhiao/dlopen-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/weizhiao/dlopen-rs/actions)
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
  <b>高性能、纯 Rust 实现的 ELF 动态链接器。</b>
</p>

<p align="center">
  <a href="README.md">English</a> | <a href="README-zh_cn.md">简体中文</a>
</p>

`dlopen-rs` 是一个功能齐全的动态链接器，完全使用 Rust 实现。它为操作动态库提供了一组 Rust 友好的接口，同时也提供了一组与 `libc` 行为一致的 C 兼容接口。

## 🚀 核心特性

- **纯 Rust 实现：** 核心加载和链接逻辑对 C 运行时零依赖。
- **`#![no_std]` 支持：** 可用于裸机环境、内核或嵌入式系统。
- **`LD_PRELOAD` 兼容：** 可以在不修改代码的情况下替换 `libc` 的 `dlopen`、`dlsym` 和 `dl_iterate_phdr`。
- **现代 API：** 为动态库管理提供安全且符合人体工程学的 Rust API。

## 🛠 用法

### 作为 Rust 库
在你的 `Cargo.toml` 中添加 `dlopen-rs`：
```toml
[dependencies]
dlopen-rs = "0.8.0"
```

### 作为 `LD_PRELOAD` 替代品

你可以使用 `dlopen-rs` 来拦截标准库调用：

```shell
# 1. 编译兼容库
$ cargo build -r -p cdylib

# 2. 编译你的应用程序/示例
$ cargo build -r -p dlopen-rs --example preload

# 3. 替换 libc 的实现
$ RUST_LOG=trace LD_PRELOAD=./target/release/libdlopen.so ./target/release/examples/preload
```

## 📊 架构支持

| 架构        | 加载支持 | 延迟绑定 | 测试状态 |
| ----------- | -------- | -------- | -------- |
| **x86_64**  | ✅        | ✅        | ✅ (CI)   |
| **aarch64** | ✅        | ✅        | 🛠️ (手动) |
| **riscv64** | ✅        | ✅        | 🛠️ (手动) |

## 💻 示例

```rust
use dlopen_rs::{ElfLibrary, OpenFlags, Result};

fn main() -> Result<()> {
    // 1. 加载库
    let lib = ElfLibrary::dlopen("./target/release/libexample.so", OpenFlags::RTLD_LAZY)?;

    // 2. 获取并调用一个简单的函数: fn(i32, i32) -> i32
    let add = unsafe { lib.get::<fn(i32, i32) -> i32>("add")? };
    println!("add(1, 1) = {}", add(1, 1));

    Ok(())
}
```

## ⚙️ 特性标志 (Feature Flags)

| 特性          | 默认开启 | 描述                                           |
| ------------- | -------- | ---------------------------------------------- |
| `use-syscall` | ❌        | 直接使用系统调用加载库（适用于 `no_std`）。    |
| `version`     | ❌        | 启用对 ELF 符号版本的支持。                    |
| `std`         | ✅        | 启用标准库集成。在 `no_std` 环境中请禁用此项。 |

## ⚖️ 许可证

基于 [Apache License 2.0](LICENSE) 许可。

## 🤝 贡献

欢迎贡献！如果你遇到问题或有性能改进的想法，请开启 Issue 或 PR。对于特定的 GDB 相关调试问题，请参考 [TroubleshootingGdb.md](TroubleshootingGdb.md)。

<a href="https://github.com/weizhiao/dlopen-rs/graphs/contributors">
  <img src="https://contributors-img.web.app/image?repo=weizhiao/dlopen-rs" alt="Project Contributors" />
</a>

---

**最低支持 Rust 版本 (MSRV):** 1.93.0 或更高。
