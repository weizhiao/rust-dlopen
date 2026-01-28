[![](https://img.shields.io/crates/v/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![](https://img.shields.io/crates/d/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![license](https://img.shields.io/crates/l/dlopen-rs.svg)](https://crates.io/crates/dlopen-rs)
[![dlopen-rs on docs.rs](https://docs.rs/dlopen-rs/badge.svg)](https://docs.rs/dlopen-rs)
[![Rust](https://img.shields.io/badge/rust-1.88.0%2B-blue.svg?maxAge=3600)](https://github.com/weizhiao/dlopen_rs)
[![Build Status](https://github.com/weizhiao/dlopen-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/weizhiao/dlopen-rs/actions)
# dlopen-rs

[文档](https://docs.rs/dlopen-rs/)

`dlopen-rs`是一个完全使用Rust实现的动态链接器，提供了一组对Rust友好的操作动态库的接口，也提供了一组与libc中行为一致的C接口。

## 用法
你可以使用`dlopen-rs`替换`libloading`来加载动态库，也可以在不修改任何代码的情况下，利用`LD_PRELOAD`将libc中的`dlopen`，`dlsym`，`dl_iterate_phdr`等函数替换为`dlopen-rs`中的实现。

```shell
# 将本库编译成动态库形式
cargo build -r -p cdylib
# 编译测试用例
cargo build -r -p dlopen-rs --example preload
# 使用本库中的实现替换libc中的实现
RUST_LOG=trace LD_PRELOAD=./target/release/libdlopen.so ./target/release/examples/preload
```

## 优势
1. 能够为 #![no_std] 目标提供加载 `ELF` 动态库的支持。
2. 能够轻松地在运行时用自己的自定义符号替换共享库中的符号。
3. 大多数情况下有比`ld.so`更快的速度。（加载动态库和获取符号）
4. 提供了对Rust友好的接口。

## 特性

| 特性    | 是否默认开启 | 描述                                         |
| ------- | ------------ | -------------------------------------------- |
| std     | 是           | 启用Rust标准库                               |
| debug   | 是           | 启用后可以使用 gdb/lldb 调试已加载的动态库。 |
| mmap    | 是           | 启用在有mmap的平台上的默认实现               |
| version     | 否           | 在寻找符号时使用符号的版本号                 |
| tls         | 是           | 启用后动态库中可以使用线程本地存储。         |
| use-syscall | 否           | 使用系统调用从文件系统中加载动态库。         |

## 指令集支持

| 指令集      | 支持 | 延迟绑定 | 测试      |
| ----------- | ---- | -------- | --------- |
| x86_64      | ✅    | ✅        | ✅(CI)     |
| aarch64     | ✅    | ✅        | ✅(Manual) |
| riscv64     | ✅    | ✅        | ✅(Manual) |
| loongarch64 | ✅    | ❌        | ❌         |

## 示例

使用`dlopen`接口加载动态库，使用`dl_iterate_phdr`接口遍历已经加载的动态库。此外本库使用了`log`库，你可以使用自己喜欢的库输出日志信息，来查看dlopen-rs的工作流程，本库的例子中使用的是`env_logger`库。
```rust
use dlopen_rs::{ElfLibrary, OpenFlags};
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

## 最低编译器版本支持
Rust 1.85.0及以上

## 未完成
* dlinfo还未实现。dlerror目前只会返回NULL。
* dlsym的RTLD_NEXT还未实现。
* 在调用dlopen失败时，新加载的动态库虽然会被销毁但没有调用.fini中的函数。
## 补充
如果在使用过程中遇到问题可以在 GitHub 上提出问题，十分欢迎大家为本库提交代码一起完善dlopen-rs的功能。😊
