use criterion::{Criterion, criterion_group, criterion_main};
use dlopen_rs::{ElfLibrary, OpenFlags};
use libloading::Library;

fn get_symbol(c: &mut Criterion) {
    dlopen_rs::init();
    let path = "./target/release/libexample.so";
    let lib1 = ElfLibrary::dlopen(path, OpenFlags::CUSTOM_NOT_REGISTER).unwrap();
    let lib2 = unsafe { Library::new(path).unwrap() };
    c.bench_function("dlopen-rs:get", |b| {
        b.iter(|| unsafe { lib1.get::<fn(i32, i32) -> i32>("add").unwrap() })
    });
    c.bench_function("libloading:get", |b| {
        b.iter(|| {
            unsafe { lib2.get::<fn(i32, i32) -> i32>("add".as_bytes()).unwrap() };
        })
    });
}

criterion_group!(benches, get_symbol);
criterion_main!(benches);
