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
  <b>纯 Rust 实现的 ELF 动态链接器、dlopen 兼容层，以及实验性的 Linux rtld。</b>
</p>

<p align="center">
  <a href="README.md">English</a> | <a href="README-zh_cn.md">简体中文</a>
</p>

`dlopen-rs` 提供了一套 Rust 友好的动态库加载 API，同时导出与 libc 兼容的 `dlopen`、`dlsym`、`dladdr`、`dl_iterate_phdr` 等接口。项目的核心目标是把 ELF 加载、依赖解析、重定位、符号查询和 TLS 管理放进 Rust 代码中，并逐步推进到可替换 glibc `ld.so` 的运行时链接器实现。

## 核心能力

- **动态库加载：** 支持从路径或内存中的 ELF 字节加载动态库。
- **依赖解析：** 支持 `DT_NEEDED`、`RPATH`、`RUNPATH`、`ld.so.cache` 和常见系统搜索路径。
- **符号解析：** 提供 Rust API 与 C ABI 两套入口，支持全局作用域、依赖作用域和 `RTLD_NEXT` / `RTLD_DEFAULT`。
- **重定位：** 支持常规动态重定位、PLT/GOT、延迟绑定和 RELR。
- **TLS：** 支持动态 TLS、静态 TLS 注册，以及 rtld 启动阶段需要的初始线程 TLS。
- **`no_std`：** 核心加载链路可在禁用 `std` 时工作，并可通过 `use-syscall` 走 Linux syscall 后端。
- **replacement rtld：** `rtld` workspace crate 正在实现可作为 `ld-linux-x86-64.so.2` 使用的动态链接器产物。

## 项目状态

| 模块 | 状态 |
| --- | --- |
| Rust `ElfLibrary` API | 可用 |
| libc 兼容 dlopen/dlsym/dladdr/dl_iterate_phdr | 可用 |
| `LD_PRELOAD` 兼容库 | 可用 |
| `no_std` + syscall 加载路径 | 可用，仍在完善边界 |
| replacement rtld | 实验中，当前重点是 x86_64 Linux/glibc 兼容 |

## 安装

在普通 Rust 程序中使用：

```toml
[dependencies]
dlopen-rs = "0.8.0"
```

禁用 `std` 并使用 syscall 后端：

```toml
[dependencies]
dlopen-rs = { version = "0.8.0", default-features = false, features = ["use-syscall"] }
```

## Rust API 示例

先构建示例动态库：

```shell
cargo build --release -p example_dylib
```

然后运行 `dlopen-rs` 示例：

```shell
cargo run --release --example dlopen
```

核心代码：

```rust
use dlopen_rs::{ElfLibrary, OpenFlags, Result};

fn main() -> Result<()> {
    let lib = ElfLibrary::dlopen("./target/release/libexample.so", OpenFlags::RTLD_LAZY)?;
    let add = unsafe { lib.get::<fn(i32, i32) -> i32>("add")? };

    println!("add(1, 1) = {}", add(1, 1));
    Ok(())
}
```

## LD_PRELOAD 兼容库

`cdylib` crate 会构建一个导出 libc dlopen 系列符号的兼容库：

```shell
cargo build --release -p cdylib
cargo build --release -p example_dylib
cargo build --release --example preload

RUST_LOG=trace \
LD_PRELOAD=./target/release/libdlopen.so \
./target/release/examples/preload
```

这适合用来验证 `dlopen-rs` 对现有程序中 `dlopen`/`dlsym` 调用的接管能力。

## replacement rtld

`rtld` crate 会构建一个实验性的 ELF interpreter，目标是逐步替换 glibc `ld-linux-x86-64.so.2`：

```shell
cargo build-rtld
```

产物位于：

```text
target/x86_64-unknown-linux-none/release/librtld.so
target/x86_64-unknown-linux-none/release/ld-linux-x86-64.so.2
```

这个路径使用 `x86_64-unknown-linux-none`、`-Z build-std=core,alloc,compiler_builtins` 和自定义 linker 参数。若当前工具链不支持 `-Z build-std`，请使用 nightly 工具链或配置对应的 Rust 工具链。

## 开发命令

仓库提供了几个 Cargo alias，用来区分 host/std 与 rtld/no_std 检查：

```shell
cargo check-host
cargo check-rtld
cargo build-rtld
```

常用验证组合：

```shell
cargo check-host
cargo check-rtld
cargo check --no-default-features --lib
cargo check --no-default-features --features use-syscall --lib
cargo test --test rtld_artifact rtld_artifact_has_interpreter_shape
```

## Feature Flags

| Feature | 默认开启 | 说明 |
| --- | --- | --- |
| `std` | 是 | 启用标准库集成、host 初始化和 ctor。 |
| `use-syscall` | 否 | 使用 Linux syscall 后端，主要用于 `no_std`/rtld 路径。 |
| `version` | 否 | 启用 ELF 符号版本支持。 |

## 架构支持

| 架构 | 动态库加载 | 延迟绑定 | replacement rtld |
| --- | --- | --- | --- |
| x86_64 Linux | 支持 | 支持 | 实验中 |
| aarch64 | 支持 | 支持 | 暂未启用 |
| riscv64 | 支持 | 部分路径仍在验证 | 暂未启用 |

## 目录结构

```text
src/              Rust API、C ABI、loader/register 核心逻辑
src/rtld.rs       no_std rtld 入口和 rtld 专用 TLS glue
src/host_init.rs  std/host 模式下从宿主 ld.so 导入已加载对象
cdylib/           LD_PRELOAD 兼容库
rtld/             replacement ld.so 产物 crate
rtld/impl/        no_std rtld 实现
example-dylib/    示例动态库
examples/         API 与 LD_PRELOAD 示例
tests/            集成测试和 rtld 产物检查
```

## 许可证

基于 [Apache License 2.0](LICENSE) 许可。

## 贡献

欢迎提交 Issue 和 PR。涉及 GDB/r_debug/link_map 的调试问题，可以参考 [TroubleshootingGdb.md](TroubleshootingGdb.md)。

<a href="https://github.com/weizhiao/dlopen-rs/graphs/contributors">
  <img src="https://contributors-img.web.app/image?repo=weizhiao/dlopen-rs" alt="Project Contributors" />
</a>

---

**最低支持 Rust 版本 (MSRV):** 1.93.0 或更高。
