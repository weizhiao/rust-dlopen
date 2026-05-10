mod support;

use criterion::{Criterion, criterion_group, criterion_main};
use dlopen_rs::{ElfLibrary, OpenFlags};
use libloading::Library;

fn load(c: &mut Criterion) {
    let path = support::example_dylib_path();
    c.bench_function("dlopen-rs:dlopen", |b| {
        b.iter(|| {
            let _libexample =
                ElfLibrary::dlopen(path.to_str().unwrap(), OpenFlags::RTLD_GLOBAL).unwrap();
        })
    });
    c.bench_function("libloading:new", |b| {
        b.iter(|| {
            unsafe { Library::new(&path).unwrap() };
        })
    });
}

criterion_group!(benches, load);
criterion_main!(benches);
