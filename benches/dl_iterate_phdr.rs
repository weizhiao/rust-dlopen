use criterion::{Criterion, criterion_group, criterion_main};
use dlopen_rs::ElfLibrary;
use libc::{dl_iterate_phdr, size_t};
use std::ptr::null_mut;

fn iterate_phdr(c: &mut Criterion) {
    dlopen_rs::init();
    unsafe extern "C" fn callback(
        _info: *mut libc::dl_phdr_info,
        _size: size_t,
        _data: *mut libc::c_void,
    ) -> libc::c_int {
        0
    }

    c.bench_function("dlopen-rs:dl_iterate_phdr", |b| {
        b.iter(|| ElfLibrary::dl_iterate_phdr(|_info| Ok(())))
    });
    c.bench_function("libc:dl_iterate_phdr", |b| {
        b.iter(|| {
            unsafe { dl_iterate_phdr(Some(callback), null_mut()) };
        })
    });
}

criterion_group!(benches, iterate_phdr);
criterion_main!(benches);
