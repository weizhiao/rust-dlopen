use criterion::{Criterion, criterion_group, criterion_main};
use dlopen_rs::{ElfLibrary, OpenFlags};
use libloading::Library;
use std::path::Path;

fn load(c: &mut Criterion) {
    dlopen_rs::init();
    let path = Path::new("./target/release/libexample.so");
    c.bench_function("dlopen-rs:dlopen", |b| {
        b.iter(|| {
            let _libexample = ElfLibrary::dlopen(path, OpenFlags::RTLD_GLOBAL).unwrap();
        })
    });
    c.bench_function("libloading:new", |b| {
        b.iter(|| {
            unsafe { Library::new(path).unwrap() };
        })
    });
}

criterion_group!(benches, load);
criterion_main!(benches);
