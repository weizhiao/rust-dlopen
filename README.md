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
  <b>A pure-Rust ELF dynamic linker, dlopen compatibility layer, and experimental Linux rtld.</b>
</p>

<p align="center">
  <a href="README.md">English</a> | <a href="README-zh_cn.md">简体中文</a>
</p>

`dlopen-rs` provides a Rust-friendly dynamic library loading API and exports libc-compatible entry points such as `dlopen`, `dlsym`, `dladdr`, and `dl_iterate_phdr`. The project moves ELF loading, dependency resolution, relocation, symbol lookup, and TLS management into Rust, while steadily growing an rtld implementation that can eventually replace glibc `ld.so`.

## Capabilities

- **Dynamic loading:** Load ELF shared objects from paths or in-memory bytes.
- **Dependency resolution:** Handle `DT_NEEDED`, `RPATH`, `RUNPATH`, `ld.so.cache`, and common system search paths.
- **Symbol lookup:** Provide Rust APIs and C ABI entry points, including global lookup, dependency scopes, `RTLD_NEXT`, and `RTLD_DEFAULT`.
- **Relocation:** Support regular dynamic relocations, PLT/GOT relocation, lazy binding, and RELR.
- **TLS:** Support dynamic TLS, static TLS registration, and initial-thread TLS needed during rtld startup.
- **`no_std`:** Run the core loading path without `std`, with an optional Linux syscall backend via `use-syscall`.
- **Replacement rtld:** Build an experimental ELF interpreter that advertises itself as `ld-linux-x86-64.so.2`.

## Project Status

| Area | Status |
| --- | --- |
| Rust `ElfLibrary` API | Usable |
| libc-compatible dlopen/dlsym/dladdr/dl_iterate_phdr | Usable |
| `LD_PRELOAD` compatibility library | Usable |
| `no_std` + syscall loading path | Usable, still being refined |
| Replacement rtld | Experimental, focused on x86_64 Linux/glibc compatibility |

## Installation

For regular Rust programs:

```toml
[dependencies]
dlopen-rs = "0.8.0"
```

For `no_std` with the syscall backend:

```toml
[dependencies]
dlopen-rs = { version = "0.8.0", default-features = false, features = ["use-syscall"] }
```

## Rust API Example

Build the example shared object first:

```shell
cargo build --release -p example_dylib
```

Then run the `dlopen-rs` example:

```shell
cargo run --release --example dlopen
```

Core usage:

```rust
use dlopen_rs::{ElfLibrary, OpenFlags, Result};

fn main() -> Result<()> {
    let lib = ElfLibrary::dlopen("./target/release/libexample.so", OpenFlags::RTLD_LAZY)?;
    let add = unsafe { lib.get::<fn(i32, i32) -> i32>("add")? };

    println!("add(1, 1) = {}", add(1, 1));
    Ok(())
}
```

## LD_PRELOAD Compatibility Library

The `cdylib` crate builds a libc dlopen-family compatibility library:

```shell
cargo build --release -p cdylib
cargo build --release -p example_dylib
cargo build --release --example preload

RUST_LOG=trace \
LD_PRELOAD=./target/release/libdlopen.so \
./target/release/examples/preload
```

This is useful for checking how well `dlopen-rs` can interpose existing `dlopen`/`dlsym` calls.

## Replacement rtld

The `rtld` crate builds an experimental ELF interpreter intended to grow into a replacement for glibc `ld-linux-x86-64.so.2`:

```shell
cargo build-rtld
```

Artifacts are written to:

```text
target/x86_64-unknown-linux-none/release/librtld.so
target/x86_64-unknown-linux-none/release/ld-linux-x86-64.so.2
```

This path uses `x86_64-unknown-linux-none`, `-Z build-std=core,alloc,compiler_builtins`, and custom linker arguments. Use a nightly toolchain, or another toolchain capable of `-Z build-std`, when building this target.

## Development Commands

The repository defines Cargo aliases that keep host/std checks separate from rtld/no_std checks:

```shell
cargo check-host
cargo check-rtld
cargo build-rtld
```

Common validation set:

```shell
cargo check-host
cargo check-rtld
cargo check --no-default-features --lib
cargo check --no-default-features --features use-syscall --lib
cargo test --test rtld_artifact rtld_artifact_has_interpreter_shape
```

## Feature Flags

| Feature | Default | Description |
| --- | --- | --- |
| `std` | Yes | Enables standard library integration, host initialization, and ctor support. |
| `use-syscall` | No | Uses the Linux syscall backend, mainly for `no_std` and rtld paths. |
| `version` | No | Enables ELF symbol version support. |

## Architecture Support

| Architecture | Dynamic Loading | Lazy Binding | Replacement rtld |
| --- | --- | --- | --- |
| x86_64 Linux | Supported | Supported | Experimental |
| aarch64 | Supported | Supported | Not enabled |
| riscv64 | Supported | Some paths still being validated | Not enabled |

## Repository Layout

```text
src/              Rust API, C ABI, loader/register core logic
src/rtld.rs       no_std rtld entry points and rtld-specific TLS glue
src/host_init.rs  std/host import of objects already loaded by the host ld.so
cdylib/           LD_PRELOAD compatibility library
rtld/             replacement ld.so artifact crate
rtld/impl/        no_std rtld implementation
example-dylib/    example shared object
examples/         API and LD_PRELOAD examples
tests/            integration tests and rtld artifact checks
```

## License

Licensed under the [Apache License 2.0](LICENSE).

## Contributing

Issues and pull requests are welcome. For GDB/r_debug/link_map related debugging notes, see [TroubleshootingGdb.md](TroubleshootingGdb.md).

<a href="https://github.com/weizhiao/dlopen-rs/graphs/contributors">
  <img src="https://contributors-img.web.app/image?repo=weizhiao/dlopen-rs" alt="Project Contributors" />
</a>

---

**Minimum Supported Rust Version (MSRV):** 1.93.0 or higher.
