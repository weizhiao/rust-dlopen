use dlopen_rs::{ElfLibrary, OpenFlags};
use std::path::Path;

fn main() {
    unsafe { std::env::set_var("RUST_LOG", "trace") };
    env_logger::init();
    dlopen_rs::init();
    let path = Path::new("./target/release/libexample.so");
    let libexample1 =
        ElfLibrary::dlopen(path, OpenFlags::CUSTOM_NOT_REGISTER | OpenFlags::RTLD_LAZY).unwrap();
    let add = unsafe { libexample1.get::<fn(i32, i32) -> i32>("add").unwrap() };
    println!("{}", add(1, 1));

    let print = unsafe { libexample1.get::<fn(&str)>("print").unwrap() };
    print("dlopen-rs: hello world");

    let args = unsafe { libexample1.get::<fn()>("args") }.unwrap();
    args();

    drop(libexample1);

    let bytes = std::fs::read(path).unwrap();
    let libexample2 = ElfLibrary::dlopen_from_binary(
        &bytes,
        "./target/release/libexample.so",
        OpenFlags::RTLD_GLOBAL,
    )
    .unwrap();

    let backtrace = unsafe { libexample2.get::<fn()>("backtrace").unwrap() };
    backtrace();

	let panic = unsafe { libexample2.get::<fn()>("panic").unwrap() };
	panic();

    let dl_info = ElfLibrary::dladdr(backtrace.into_raw() as usize).unwrap();
    println!("{:?}", dl_info);

    ElfLibrary::dl_iterate_phdr(|info| {
        println!("iterate dynamic library: {}", info.name());
        Ok(())
    })
    .unwrap();
}
