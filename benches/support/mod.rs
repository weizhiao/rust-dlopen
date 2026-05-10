use std::{
    env,
    path::PathBuf,
    sync::{Once, OnceLock},
};

static BUILD_EXAMPLE: Once = Once::new();
static TARGET_TRIPLE: OnceLock<&'static str> = OnceLock::new();

pub(crate) fn example_dylib_path() -> PathBuf {
    BUILD_EXAMPLE.call_once(|| {
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("build")
            .arg("-r")
            .arg("-p")
            .arg("example_dylib")
            .env("CARGO_PROFILE_RELEASE_PANIC", "unwind")
            .arg("--target")
            .arg(target_triple());
        assert!(
            cmd.status()
                .expect("could not compile the benchmark helper")
                .success()
        );
    });

    target_dir()
        .join(target_triple())
        .join("release")
        .join("libexample.so")
}

fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn target_triple() -> &'static str {
    TARGET_TRIPLE.get_or_init(|| match env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-gnu",
        "aarch64" => "aarch64-unknown-linux-gnu",
        "riscv64" => "riscv64gc-unknown-linux-gnu",
        "loongarch64" => "loongarch64-unknown-linux-musl",
        arch => panic!("unsupported benchmark arch: {arch}"),
    })
}
