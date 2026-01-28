use dlopen_rs::ElfLibrary;

#[test]
fn test_cache_lookup() {
    let _ = env_logger::try_init();
    dlopen_rs::init();
    // libm.so.6 is almost always in /etc/ld.so.cache but usually not linked to the test runner unless explicitly requested.
    let res = ElfLibrary::dlopen("libm.so.6", dlopen_rs::OpenFlags::RTLD_NOW);
    assert!(res.is_ok(), "Failed to load libm.so.6 from cache: {:?}", res.err());
}
