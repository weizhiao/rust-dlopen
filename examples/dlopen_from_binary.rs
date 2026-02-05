use dlopen_rs::{ElfLibrary, OpenFlags};

fn main() -> Result<(), String> {
    // Set RUST_LOG=trace if not already set to see loader details
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "trace") };
    }
    env_logger::init();

    let path = "./target/release/libexample.so";
    println!("Loading library from memory: {:?}", path);

    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;

    // Load library from memory buffer
    // The second argument is a virtual path used for identification (e.g. in backtraces)
    let lib = ElfLibrary::dlopen_from_binary(&bytes, path, OpenFlags::RTLD_GLOBAL)
        .map_err(|e| e.to_string())?;

    // Call functions from the loaded library
    let backtrace = unsafe { lib.get::<fn()>("backtrace").map_err(|e| e.to_string())? };
    println!("--- Calling backtrace ---");
    backtrace();

    let thread_local = unsafe { lib.get::<fn()>("thread_local").map_err(|e| e.to_string())? };
    println!("\n--- Calling thread_local ---");
    thread_local();

    let panic_func = unsafe { lib.get::<fn()>("panic").map_err(|e| e.to_string())? };
    println!("\n--- Calling panic (caught inside) ---");
    panic_func();

    let print = unsafe { lib.get::<fn(&str)>("print").map_err(|e| e.to_string())? };
    print("Hello from dlopen-rs!");

    let args = unsafe { lib.get::<fn()>("args").map_err(|e| e.to_string())? };
    println!("Calling args():");
    args();

    Ok(())
}
