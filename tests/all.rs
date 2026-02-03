use dlopen_rs::{ElfLibrary, OpenFlags};
use std::env::consts;
use std::path::PathBuf;
use std::sync::OnceLock;

const TARGET_DIR: Option<&'static str> = option_env!("CARGO_TARGET_DIR");
static TARGET_TRIPLE: OnceLock<String> = OnceLock::new();

fn lib_path(file_name: &str) -> String {
    let path: PathBuf = TARGET_DIR.unwrap_or("target").into();
    path.join(TARGET_TRIPLE.get().unwrap())
        .join("release")
        .join(file_name)
        .to_str()
        .unwrap()
        .to_string()
}

const PACKAGE_NAME: [&str; 1] = ["example_dylib"];

fn compile() {
    static ONCE: ::std::sync::Once = ::std::sync::Once::new();
    ONCE.call_once(|| {
        unsafe { std::env::set_var("RUST_LOG", "trace") };
        env_logger::init();
        dlopen_rs::init();
        let arch = consts::ARCH;
        if arch.contains("x86_64") {
            TARGET_TRIPLE
                .set("x86_64-unknown-linux-gnu".to_string())
                .unwrap();
        } else if arch.contains("riscv64") {
            TARGET_TRIPLE
                .set("riscv64gc-unknown-linux-gnu".to_string())
                .unwrap();
        } else if arch.contains("aarch64") {
            TARGET_TRIPLE
                .set("aarch64-unknown-linux-gnu".to_string())
                .unwrap();
        } else if arch.contains("loongarch64") {
            TARGET_TRIPLE
                .set("loongarch64-unknown-linux-musl".to_string())
                .unwrap();
        } else {
            unimplemented!()
        }

        for name in PACKAGE_NAME {
            let mut cmd = ::std::process::Command::new("cargo");
            cmd.arg("build")
                .arg("-r")
                .arg("-p")
                .arg(name)
                .arg("--target")
                .arg(TARGET_TRIPLE.get().unwrap().as_str());
            assert!(
                cmd.status()
                    .expect("could not compile the test helpers!")
                    .success()
            );
        }

        let libexample = lib_path("libexample.so");
        let _ = std::fs::copy(&libexample, lib_path("libpromotion.so"));
        let _ = std::fs::copy(&libexample, lib_path("libnodelete.so"));
        let _ = std::fs::copy(&libexample, lib_path("libexample_noload.so"));
    });
}

#[test]
fn dlopen() {
    compile();
    let path = lib_path("libexample.so");
    assert!(ElfLibrary::dlopen(path, OpenFlags::RTLD_NOW).is_ok());
}

#[test]
fn dl_iterate_phdr() {
    compile();
    let path = lib_path("libexample.so");
    let _lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_NOW).unwrap();
    ElfLibrary::dl_iterate_phdr(|info| {
        println!("iterate dynamic library: {}", info.name());
        Ok(())
    })
    .unwrap();
}

#[test]
fn panic() {
    compile();
    let path = lib_path("libexample.so");
    let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_NOW).unwrap();
    let panic = unsafe { lib.get::<fn()>("panic").unwrap() };
    panic();
}

#[test]
fn rtld_noload() {
    compile();
    let path = lib_path("libexample_noload.so");

    // Should fail if not loaded
    assert!(ElfLibrary::dlopen(&path, OpenFlags::RTLD_NOLOAD).is_err());

    // Load it
    let _lib = ElfLibrary::dlopen(&path, OpenFlags::RTLD_LOCAL).unwrap();

    // Should succeed now
    assert!(ElfLibrary::dlopen(&path, OpenFlags::RTLD_NOLOAD).is_ok());

    // Should succeed with promotion
    let lib_global =
        ElfLibrary::dlopen(&path, OpenFlags::RTLD_NOLOAD | OpenFlags::RTLD_GLOBAL).unwrap();
    assert!(lib_global.flags().contains(OpenFlags::RTLD_GLOBAL));
}

#[test]
fn promotion() {
    compile();
    let path = lib_path("libpromotion.so");

    // 1. Load with RTLD_LOCAL
    let lib_local = ElfLibrary::dlopen(&path, OpenFlags::RTLD_LAZY).unwrap();
    assert!(!lib_local.flags().contains(OpenFlags::RTLD_GLOBAL));

    // Symbol should NOT be in global scope
    assert!(dlopen_rs::dlsym_default::<fn(i32, i32) -> i32>("add").is_err());

    // 2. Promote to RTLD_GLOBAL
    let lib_promoted =
        ElfLibrary::dlopen(&path, OpenFlags::RTLD_LAZY | OpenFlags::RTLD_GLOBAL).unwrap();
    assert!(lib_promoted.flags().contains(OpenFlags::RTLD_GLOBAL));

    // Symbol SHOULD be in global scope now
    let add_sym = dlopen_rs::dlsym_default::<fn(i32, i32) -> i32>("add")
        .expect("Symbol should be available after promotion");
    assert_eq!(add_sym(1, 2), 3);
}

#[test]
fn nodelete() {
    compile();
    let path = lib_path("libnodelete.so");

    let lib = ElfLibrary::dlopen(&path, OpenFlags::RTLD_LAZY).unwrap();
    assert!(!lib.flags().contains(OpenFlags::RTLD_NODELETE));

    // Promote to RTLD_NODELETE
    let lib_nodelete =
        ElfLibrary::dlopen(&path, OpenFlags::RTLD_LAZY | OpenFlags::RTLD_NODELETE).unwrap();
    assert!(lib_nodelete.flags().contains(OpenFlags::RTLD_NODELETE));
}

#[test]
fn dladdr() {
    compile();
    let path = lib_path("libexample.so");
    let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_NOW).unwrap();
    let print = unsafe { lib.get::<fn(&str)>("print").unwrap() };
    let find = ElfLibrary::dladdr(print.into_raw() as usize).unwrap();
    assert!(find.dylib().name() == lib.name());
}

#[test]
fn thread_local() {
    compile();
    let path = lib_path("libexample.so");
    let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_NOW).unwrap();
    let thread_local = unsafe { lib.get::<fn()>("thread_local").unwrap() };
    thread_local();
}
