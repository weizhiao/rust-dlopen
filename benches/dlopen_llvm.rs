use criterion::{Criterion, criterion_group, criterion_main};
use dlopen_rs::{ElfLibrary, OpenFlags};
use std::path::Path;

fn load(c: &mut Criterion) {
    let path = "/usr/lib/llvm-18/lib/libLLVM-18.so";
    if !Path::new(path).exists() {
        eprintln!("skipping LLVM dlopen benchmark because {path} is not available");
        return;
    }
    c.bench_function("dlopen-rs:dlopen", |b| {
        b.iter(|| {
            let _libexample = ElfLibrary::dlopen(path, OpenFlags::RTLD_GLOBAL).unwrap();
        })
    });
}

criterion_group!(benches, load);
criterion_main!(benches);
