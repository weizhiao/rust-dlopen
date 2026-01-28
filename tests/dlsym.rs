use dlopen_rs::{ElfLibrary, OpenFlags, dlsym_default, dlsym_next};
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
        dlopen_rs::init();
        let arch = consts::ARCH;
        if arch.contains("x86_64") {
            TARGET_TRIPLE
                .set("x86_64-unknown-linux-gnu".to_string())
                .unwrap();
        } else if arch.contains("aarch64") {
            TARGET_TRIPLE
                .set("aarch64-unknown-linux-gnu".to_string())
                .unwrap();
        } else if arch.contains("riscv64") {
            TARGET_TRIPLE
                .set("riscv64gc-unknown-linux-gnu".to_string())
                .unwrap();
        }

        for name in PACKAGE_NAME {
            let mut cmd = std::process::Command::new("cargo");
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
    });
}

#[unsafe(no_mangle)]
pub fn add(a: i32, b: i32) -> i32 {
    a + b + 100
}

#[test]
fn test_dlsym_next() {
    compile();
    let path = lib_path("libexample.so");
    let _lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NOW).unwrap();

    // dlsym_next should skip the local "add" (which returns a+b+100)
    // and find the one in libexample.so (which returns a+b).
    let add_func = unsafe { dlsym_next::<fn(i32, i32) -> i32>("add").unwrap() };

    // If it found the local one, it would be 103.
    // If it correctly found the next one, it should be 3.
    assert_eq!(add_func(1, 2), 3);

    // Verify it's not the same as local add
    assert_ne!(add_func.into_raw() as usize, add as *const () as usize);

    // Test non-existent symbol
    let non_existent = unsafe { dlsym_next::<fn()>("non_existent_symbol") };
    assert!(non_existent.is_err());
}

#[test]
fn test_dlsym_default() {
    compile();
    let path = lib_path("libexample.so");
    // Must be RTLD_GLOBAL to be found by dlsym_default
    let _lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_GLOBAL | OpenFlags::RTLD_NOW).unwrap();

    let add = dlsym_default::<fn(i32, i32) -> i32>("add").unwrap();
    assert_eq!(add(10, 20), 30);
}

#[test]
fn test_dlsym() {
    compile();
    let path = lib_path("libexample.so");
    let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_GLOBAL).unwrap();
    let print = unsafe { lib.get::<fn(&str)>("print").unwrap() };
    print("dlopen-rs: hello world");
}
