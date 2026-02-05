use dlopen_rs::{ElfLibrary, OpenFlags};

fn main() -> Result<(), String> {
    // Set RUST_LOG=trace if not already set to see loader details
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "trace") };
    }
    env_logger::init();

    let path = "./target/release/libexample.so";
    let lib = ElfLibrary::dlopen(path, OpenFlags::RTLD_LAZY).map_err(|e| e.to_string())?;

    println!("Examining loaded library: {:?}", path);

    // 1. dladdr - find information about a symbol address
    let add_symbol = unsafe {
        lib.get::<fn(i32, i32) -> i32>("add")
            .map_err(|e| e.to_string())?
    };
    let addr = add_symbol.into_raw() as usize;

    if let Some(info) = ElfLibrary::dladdr(addr) {
        println!("\nSymbol info for 'add':");
        println!("  File: {}", info.dylib().name());
        if let Some(sname) = info.symbol_name() {
            println!("  Symbol: {}", sname);
        }
        println!("  Base address: {:#x}", info.dylib().base());
        println!("  Symbol address: {:#x}", info.symbol_addr().unwrap_or(0));
    }

    // 2. dl_iterate_phdr - iterate over all loaded ELF objects
    println!("\nIterating over loaded dynamic libraries:");
    ElfLibrary::dl_iterate_phdr(|info| {
        println!(
            "  - {} (at {:#x}, {} segments)",
            if info.name().is_empty() {
                "[Main Executable]"
            } else {
                info.name()
            },
            info.base(),
            info.phdrs().len()
        );
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    Ok(())
}
