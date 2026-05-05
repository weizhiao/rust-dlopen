use std::{
    env, fs,
    path::{Path, PathBuf},
};

const ARTIFACT_NAME: &str = "librtld.so";
const SONAME: &str = "ld-linux-x86-64.so.2";

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let version_script = manifest_dir.join("rtld.version");

    println!("cargo:rerun-if-changed={}", version_script.display());

    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64")
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("")
    {
        return;
    }

    emit_cdylib_link_args(&version_script);
    create_soname_link(profile_dir());
}

fn emit_cdylib_link_args(version_script: &Path) {
    for arg in [
        "-e".to_owned(),
        "_start".to_owned(),
        format!("-soname={SONAME}"),
        "-Bsymbolic".to_owned(),
        "-z".to_owned(),
        "now".to_owned(),
        "-z".to_owned(),
        "relro".to_owned(),
        "--hash-style=gnu".to_owned(),
        "--allow-multiple-definition".to_owned(),
        "--undefined=_start".to_owned(),
        format!("--version-script={}", version_script.display()),
    ] {
        println!("cargo:rustc-link-arg-cdylib={arg}");
    }
}

fn profile_dir() -> PathBuf {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should be under target/<target>/<profile>/build")
        .to_owned()
}

fn create_soname_link(profile_dir: PathBuf) {
    let soname = profile_dir.join(SONAME);

    if fs::read_link(&soname).ok().as_deref() == Some(Path::new(ARTIFACT_NAME)) {
        return;
    }

    let _ = fs::remove_file(&soname);
    symlink(ARTIFACT_NAME, &soname).expect("failed to create ld-linux soname symlink");
}

#[cfg(unix)]
fn symlink(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(not(unix))]
fn symlink(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::copy(src, dst).map(|_| ())
}
