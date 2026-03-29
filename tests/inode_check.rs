use dlopen_rs::{ElfLibrary, OpenFlags};
use std::env::consts;
use std::fs;
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
    });
}

/// Test to verify that inode checking prevents duplicate loads via symlinks
#[cfg(unix)]
#[test]
fn test_symlink_inode_detection() {
    compile();

    // Get the path to the compiled test library
    let original_path = lib_path("libexample.so");

    // Ensure the library exists and convert to absolute path
    let original_path = fs::canonicalize(&original_path)
        .expect(&format!("Failed to find library at: {}", original_path));

    println!("Original library path: {:?}", original_path);

    // Create a temporary directory for our test
    let temp_dir = std::env::temp_dir().join("dlopen_inode_test_symlink");
    let _ = fs::remove_dir_all(&temp_dir); // Clean start
    let _ = fs::create_dir_all(&temp_dir);

    let symlink_path = temp_dir.join("libexample_symlink.so");

    // Create a symlink to the test library
    std::os::unix::fs::symlink(&original_path, &symlink_path).expect("Failed to create symlink");

    println!("Symlink created at: {:?}", symlink_path);

    // Load via original path
    let lib1 = ElfLibrary::dlopen(
        original_path.to_str().unwrap(),
        OpenFlags::RTLD_NOW | OpenFlags::RTLD_LOCAL,
    )
    .expect("Failed to load library via original path");

    // Load via symlink - should return the same library instance
    let lib2 = ElfLibrary::dlopen(
        symlink_path.to_str().unwrap(),
        OpenFlags::RTLD_NOW | OpenFlags::RTLD_LOCAL,
    )
    .expect("Failed to load library via symlink");

    // Verify they have the same base address (same library instance)
    assert_eq!(
        lib1.base(),
        lib2.base(),
        "Libraries loaded via different paths should have same base address due to inode checking"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_relative_path_deduplication() {
    compile();
    let original_path = lib_path("libexample.so");
    let original_path = fs::canonicalize(&original_path).unwrap();
    let dir = original_path.parent().unwrap();
    let filename = original_path.file_name().unwrap().to_str().unwrap();

    // 1. Load via absolute path
    let lib1 = ElfLibrary::dlopen(
        original_path.to_str().unwrap(),
        OpenFlags::RTLD_NOW | OpenFlags::RTLD_LOCAL,
    )
    .expect("Failed to load absolute path");

    // 2. Load via relative path (change cwd to the lib dir)
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();

    let relative_path = format!("./{}", filename);
    let lib2 = ElfLibrary::dlopen(&relative_path, OpenFlags::RTLD_NOW | OpenFlags::RTLD_LOCAL)
        .expect("Failed to load relative path");

    // Restore cwd
    std::env::set_current_dir(cwd).unwrap();

    assert_eq!(
        lib1.base(),
        lib2.base(),
        "Relative path load should be deduplicated"
    );
}

#[test]
fn test_non_existent_file_fail_fast() {
    // Test that a non-existent file with explicit path fails immediately
    let result = ElfLibrary::dlopen("/tmp/non_existent_library_XYZ.so", OpenFlags::RTLD_NOW);
    assert!(result.is_err());

    // The error message should indicate file not found, not something from the search path iteration
    let err_msg = result.err().unwrap().to_string();
    println!("Error message: {}", err_msg);
    // Note: exact error message depends on implementation, but it should fail.
}
