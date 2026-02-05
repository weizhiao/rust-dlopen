use dlopen_rs::{ElfLibrary, OpenFlags, Result};

fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "trace") };
    }
    env_logger::init();

    // 1. Load the library
    let lib = ElfLibrary::dlopen("./target/release/libexample.so", OpenFlags::RTLD_LAZY)?;

    // 2. Get and call a simple function: fn(i32, i32) -> i32
    let add = unsafe { lib.get::<fn(i32, i32) -> i32>("add")? };
    println!("add(1, 1) = {}", add(1, 1));

    Ok(())
}
