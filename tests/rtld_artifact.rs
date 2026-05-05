#![cfg(target_arch = "x86_64")]

use std::{fs, path::PathBuf, process::Command};

const RTLD_TARGET: &str = "x86_64-unknown-linux-none";

fn target_dir() -> PathBuf {
    option_env!("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn rtld_path() -> PathBuf {
    target_dir()
        .join(RTLD_TARGET)
        .join("release")
        .join("librtld.so")
}

fn rtld_interp_path() -> PathBuf {
    target_dir()
        .join(RTLD_TARGET)
        .join("release")
        .join("ld-linux-x86-64.so.2")
}

fn has_command(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn command_output(program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run {program}: {err}"));
    assert!(
        output.status.success(),
        "{program} {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("command output must be utf-8")
}

fn build_rtld() -> PathBuf {
    let status = Command::new("cargo")
        .args([
            "-Z",
            "build-std=core,alloc,compiler_builtins",
            "build",
            "-p",
            "rtld",
            "--release",
            "--target",
            RTLD_TARGET,
        ])
        .status()
        .expect("failed to invoke cargo");
    assert!(status.success(), "rtld release build failed");
    rtld_path()
}

fn test_work_dir(name: &str) -> PathBuf {
    let dir = target_dir().join("rtld-tests").join(name);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn rtld_artifact_has_interpreter_shape() {
    let path = build_rtld();
    assert!(path.exists(), "missing artifact at {}", path.display());
    let path = path.to_str().unwrap();

    let headers = command_output("readelf", &["-h", path]);
    let entry_line = headers
        .lines()
        .find(|line| line.contains("Entry point address:"))
        .expect("readelf -h should include entry point");
    assert!(
        !entry_line.ends_with("0x0"),
        "interpreter entry must be nonzero: {entry_line}"
    );

    let dynamic = command_output("readelf", &["-d", path]);
    assert!(
        dynamic.contains("Library soname: [ld-linux-x86-64.so.2]"),
        "interpreter must advertise ld-linux soname\n{dynamic}"
    );
    assert!(
        !dynamic.contains("(NEEDED)"),
        "interpreter artifact must be self-contained\n{dynamic}"
    );
    let relocations = command_output("readelf", &["-r", path]);
    assert!(
        relocations.contains("R_X86_64_RELATIVE"),
        "interpreter should exercise its own RELATIVE relocation path\n{relocations}"
    );
    assert!(
        !relocations
            .lines()
            .any(|line| line.contains("R_X86_64_") && !line.contains("R_X86_64_RELATIVE")),
        "stage-0 interpreter may only need RELATIVE relocations\n{relocations}"
    );

    let symbols = command_output("readelf", &["-Ws", path]);
    for symbol in [
        "_r_debug",
        "_dl_debug_state",
        "__tls_get_addr",
        "_dl_find_object",
        "_dl_find_dso_for_object",
        "_rtld_global",
        "_rtld_global_ro",
        "_dl_argv",
    ] {
        assert!(symbols.contains(symbol), "missing symbol {symbol}");
    }
    assert!(
        !symbols.contains("__dlopen_rtld_bootstrap_state"),
        "bootstrap state should be passed internally, not exported\n{symbols}"
    );
    for version in [
        "GLIBC_2.2.5",
        "GLIBC_2.3",
        "GLIBC_2.34",
        "GLIBC_2.35",
        "GLIBC_PRIVATE",
    ] {
        assert!(symbols.contains(version), "missing version {version}");
    }
    assert!(
        symbols.lines().any(|line| {
            line.contains("_rtld_global@@GLIBC_PRIVATE")
                && line.split_whitespace().nth(2) == Some("4352")
        }),
        "_rtld_global must keep glibc's x86_64 size\n{symbols}"
    );
    assert!(
        symbols.lines().any(|line| {
            line.contains("_rtld_global_ro@@GLIBC_PRIVATE")
                && line.split_whitespace().nth(2) == Some("952")
        }),
        "_rtld_global_ro must keep glibc's x86_64 size\n{symbols}"
    );
}

#[test]
fn rtld_artifact_can_be_loaded_as_pt_interp() {
    if !has_command("cc") || !has_command("patchelf") {
        eprintln!("skipping PT_INTERP smoke test because cc or patchelf is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("pt-interp");
    let source = dir.join("hello.c");
    let program = dir.join("hello");
    fs::write(
        &source,
        br#"
int main(void) {
    return 0;
}
"#,
    )
    .unwrap();

    assert!(
        Command::new("cc")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile test program")
            .success(),
        "failed to compile test program"
    );
    assert!(
        Command::new("patchelf")
            .arg("--set-interpreter")
            .arg(&interp)
            .arg(&program)
            .status()
            .expect("failed to patch interpreter")
            .success(),
        "failed to patch interpreter"
    );

    let output = Command::new(&program)
        .output()
        .expect("failed to execute patched program");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn rtld_artifact_can_start_glibc_program() {
    if !has_command("cc") {
        eprintln!("skipping glibc PT_INTERP test because cc is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("pt-interp-glibc");
    let source = dir.join("libc_exit.c");
    let program = dir.join("libc_exit");
    fs::write(
        &source,
        br#"
int main(void) {
    return 33;
}
"#,
    )
    .unwrap();

    assert!(
        Command::new("cc")
            .arg(&source)
            .arg(format!("-Wl,--dynamic-linker={}", interp.display()))
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile glibc test program")
            .success(),
        "failed to compile glibc test program"
    );

    let dynamic = command_output("readelf", &["-d", program.to_str().unwrap()]);
    assert!(
        dynamic.contains("(NEEDED)") && dynamic.contains("libc.so.6"),
        "test program must need libc\n{dynamic}"
    );

    let output = Command::new(&program)
        .output()
        .expect("failed to execute glibc test program");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(33));
}

#[test]
fn rtld_artifact_can_run_simple_c_program() {
    if !has_command("cc") {
        eprintln!("skipping simple C PT_INTERP test because cc is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("pt-interp-simple-c");
    let source = dir.join("simple.c");
    let program = dir.join("simple");
    fs::write(
        &source,
        br#"
#include <stdio.h>

static int value = 5;

int main(void) {
    printf("rtld:%d\n", value + 2);
    return value + 2;
}
"#,
    )
    .unwrap();

    assert!(
        Command::new("cc")
            .arg(&source)
            .arg(format!("-Wl,--dynamic-linker={}", interp.display()))
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile simple C test program")
            .success(),
        "failed to compile simple C test program"
    );

    let dynamic = command_output("readelf", &["-d", program.to_str().unwrap()]);
    assert!(
        dynamic.contains("(NEEDED)") && dynamic.contains("libc.so.6"),
        "test program must need libc\n{dynamic}"
    );

    let output = Command::new(&program)
        .output()
        .expect("failed to execute simple C test program");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "rtld:7\n");
    assert_eq!(output.status.code(), Some(7));
}

#[test]
fn rtld_artifact_publishes_glibc_rtld_globals() {
    if !has_command("cc") {
        eprintln!("skipping rtld globals ABI test because cc is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("pt-interp-rtld-globals");
    let source = dir.join("globals.c");
    let program = dir.join("globals");
    fs::write(
        &source,
        br#"
#define _GNU_SOURCE
#include <link.h>
#include <stdio.h>
#include <unistd.h>

static int seen;

static int callback(struct dl_phdr_info *info, size_t size, void *data) {
    (void) size;
    (void) data;
    if (info->dlpi_phdr != 0 && info->dlpi_phnum != 0) {
        ++seen;
    }
    return 0;
}

int main(void) {
    int pagesize = getpagesize();
    long sys_pagesize = sysconf(_SC_PAGESIZE);
    long clktck = sysconf(_SC_CLK_TCK);

    if (pagesize <= 0 || sys_pagesize != pagesize || clktck <= 0) {
        return 10;
    }
    if (dl_iterate_phdr(callback, 0) != 0 || seen == 0) {
        return 11;
    }

    printf("globals:%d:%ld:%d\n", pagesize, clktck, seen);
    return 0;
}
"#,
    )
    .unwrap();

    assert!(
        Command::new("cc")
            .arg(&source)
            .arg(format!("-Wl,--dynamic-linker={}", interp.display()))
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile rtld globals ABI test program")
            .success(),
        "failed to compile rtld globals ABI test program"
    );

    let output = Command::new(&program)
        .output()
        .expect("failed to execute rtld globals ABI test program");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "rtld globals ABI test failed with {:?}\nstdout:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("globals:"),
        "unexpected stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn rtld_artifact_has_command_line_help_and_version() {
    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let help = Command::new(&interp)
        .arg("--help")
        .output()
        .expect("failed to execute rtld --help");
    assert!(
        help.status.success(),
        "rtld --help failed with {:?}",
        help.status.code()
    );
    assert!(
        help.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&help.stderr)
    );
    let help = String::from_utf8(help.stdout).expect("help output must be utf-8");
    assert!(help.contains("Usage:"), "{help}");
    assert!(help.contains("--list"), "{help}");
    assert!(help.contains("--verify"), "{help}");
    assert!(help.contains("ld-linux-x86-64.so.2"), "{help}");

    let version = Command::new(&interp)
        .arg("--version")
        .output()
        .expect("failed to execute rtld --version");
    assert!(
        version.status.success(),
        "rtld --version failed with {:?}",
        version.status.code()
    );
    assert!(
        version.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&version.stderr)
    );
    let version = String::from_utf8(version.stdout).expect("version output must be utf-8");
    assert!(version.contains("dlopen-rs rtld"), "{version}");
}

#[test]
fn rtld_artifact_command_line_can_verify_and_list_simple_c_program() {
    if !has_command("cc") {
        eprintln!("skipping rtld CLI test because cc is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("cli-simple-c");
    let source = dir.join("cli.c");
    let program = dir.join("cli");
    fs::write(
        &source,
        br#"
#include <stdio.h>

int main(void) {
    puts("cli");
    return 0;
}
"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile rtld CLI test program")
            .success(),
        "failed to compile rtld CLI test program"
    );

    let verify = Command::new(&interp)
        .arg("--verify")
        .arg(&program)
        .output()
        .expect("failed to execute rtld --verify");
    assert!(
        verify.status.success(),
        "rtld --verify failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );

    let list = Command::new(&interp)
        .arg("--list")
        .arg(&program)
        .output()
        .expect("failed to execute rtld --list");
    assert!(
        list.status.success(),
        "rtld --list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(
        list.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list = String::from_utf8(list.stdout).expect("list output must be utf-8");
    assert!(list.contains("libc.so.6"), "{list}");
    assert!(list.contains("ld-linux-x86-64.so.2"), "{list}");
}

#[test]
fn rtld_artifact_command_line_can_run_simple_c_program_directly() {
    if !has_command("cc") {
        eprintln!("skipping direct rtld CLI execution test because cc is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("cli-direct-simple-c");
    let source = dir.join("direct.c");
    let program = dir.join("direct");
    fs::write(
        &source,
        br#"
#include <stdio.h>

int main(int argc, char **argv) {
    printf("direct:%d:%s\n", argc, argv[0]);
    return argc + 7;
}
"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile direct rtld CLI test program")
            .success(),
        "failed to compile direct rtld CLI test program"
    );

    let output = Command::new(&interp)
        .arg("--argv0")
        .arg("custom-argv0")
        .arg(&program)
        .arg("payload")
        .output()
        .expect("failed to execute program through rtld CLI");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "direct:2:custom-argv0\n"
    );
    assert_eq!(output.status.code(), Some(9));
}

#[test]
fn rtld_artifact_can_tail_jump_dependency_free_program() {
    if !has_command("cc") {
        eprintln!("skipping dependency-free PT_INTERP test because cc is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("pt-interp-no-needed");
    let source = dir.join("exit42.S");
    let program = dir.join("exit42");
    fs::write(
        &source,
        br#"
    .section .text,"ax",@progbits
    .globl _start
    .type _start,@function
_start:
    movl $42, %edi
    movl $60, %eax
    syscall
    .size _start, . - _start
"#,
    )
    .unwrap();

    assert!(
        Command::new("cc")
            .arg("-nostdlib")
            .arg("-fPIE")
            .arg("-pie")
            .arg(format!("-Wl,--dynamic-linker={}", interp.display()))
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile dependency-free test program")
            .success(),
        "failed to compile dependency-free test program"
    );

    let dynamic = command_output("readelf", &["-d", program.to_str().unwrap()]);
    assert!(
        !dynamic.contains("(NEEDED)"),
        "test program must not need shared libraries\n{dynamic}"
    );

    let output = Command::new(&program)
        .output()
        .expect("failed to execute dependency-free patched program");
    assert_eq!(output.status.code(), Some(42));
}

#[test]
fn rtld_artifact_handles_main_relocations_with_full_loader() {
    if !has_command("cc") {
        eprintln!("skipping dependency-free relocation test because cc is unavailable");
        return;
    }

    let _artifact = build_rtld();
    let interp = rtld_interp_path();
    assert!(interp.exists(), "missing {}", interp.display());

    let dir = test_work_dir("pt-interp-relative-reloc");
    let source = dir.join("relative.S");
    let program = dir.join("relative");
    fs::write(
        &source,
        br#"
    .section .data,"aw",@progbits
value:
    .quad 42
anchor:
    .quad value

    .section .text,"ax",@progbits
    .globl _start
    .type _start,@function
_start:
    movq anchor(%rip), %rax
    movq (%rax), %rdi
    movl $60, %eax
    syscall
    .size _start, . - _start
"#,
    )
    .unwrap();

    assert!(
        Command::new("cc")
            .arg("-nostdlib")
            .arg("-fPIE")
            .arg("-pie")
            .arg(format!("-Wl,--dynamic-linker={}", interp.display()))
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .status()
            .expect("failed to compile dependency-free relocation test program")
            .success(),
        "failed to compile dependency-free relocation test program"
    );

    let dynamic = command_output("readelf", &["-d", program.to_str().unwrap()]);
    assert!(
        !dynamic.contains("(NEEDED)"),
        "test program must not need shared libraries\n{dynamic}"
    );
    let relocations = command_output("readelf", &["-r", program.to_str().unwrap()]);
    assert!(
        relocations.contains("R_X86_64_RELATIVE"),
        "test program must exercise relative relocations\n{relocations}"
    );

    let output = Command::new(&program)
        .output()
        .expect("failed to execute dependency-free relocation test program");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(42));
}
