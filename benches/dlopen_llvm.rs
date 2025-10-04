use criterion::{Criterion, criterion_group, criterion_main};
use dlopen_rs::{ElfLibrary, OpenFlags};

fn load(c: &mut Criterion) {
    dlopen_rs::init();
    let path = "/usr/lib/llvm-18/lib/libLLVM-18.so";
    c.bench_function("dlopen-rs:dlopen", |b| {
        b.iter(|| {
            let _libexample = ElfLibrary::dlopen(path, OpenFlags::RTLD_GLOBAL).unwrap();
        })
    });
}

criterion_group!(benches, load);
criterion_main!(benches);
