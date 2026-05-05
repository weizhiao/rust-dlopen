#![no_std]

pub const ARTIFACT_NAME: &str = "librtld.so";

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[used]
static FORCE_LINK_RTLD_IMPL: fn() = rtld_impl::force_link;
