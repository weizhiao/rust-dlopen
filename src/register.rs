use crate::{Dylib, OpenFlags};
use alloc::{borrow::ToOwned, boxed::Box, string::String, sync::Arc};
use elf_loader::RelocatedDylib;
use indexmap::IndexMap;
use spin::{Lazy, RwLock};

impl Drop for Dylib {
    fn drop(&mut self) {
        if self.flags.contains(OpenFlags::RTLD_NODELETE)
            | self.flags.contains(OpenFlags::CUSTOM_NOT_REGISTER)
        {
            return;
        }
        let mut lock = MANAGER.write();
        let ref_count = self.inner.strong_count();
        // Dylib本身 + 全局
        let threshold =
            2 + self.deps.is_some() as usize + self.flags.contains(OpenFlags::RTLD_GLOBAL) as usize;
        if ref_count == threshold {
            log::info!("Destroying dylib [{}]", self.inner.shortname());
            lock.all.shift_remove(self.inner.shortname());
            if self.flags.contains(OpenFlags::RTLD_GLOBAL) {
                lock.global.shift_remove(self.inner.shortname());
            }
            for dep in self.deps.as_ref().unwrap().iter().skip(1) {
                let dep_threshold = if let Some(lib) = lock.all.get(dep.shortname()) {
                    if lib.flags.contains(OpenFlags::RTLD_NODELETE) {
                        continue;
                    }
                    2 + lib.deps.is_some() as usize
                        + lib.flags.contains(OpenFlags::RTLD_GLOBAL) as usize
                } else {
                    continue;
                };
                if dep.strong_count() == dep_threshold {
                    log::info!("Destroying dylib [{}]", dep.shortname());
                    lock.all.shift_remove(dep.shortname());
                    lock.global.shift_remove(self.inner.shortname());
                }
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct DylibState(u8);

impl DylibState {
    const USED_MASK: u8 = 0b10000000;
    const RELOCATED: u8 = 0b01111111;

    #[inline]
    pub(crate) fn set_unused(&mut self) -> &mut Self {
        self.0 &= !Self::USED_MASK;
        self
    }

    #[inline]
    pub(crate) fn set_used(&mut self) -> &mut Self {
        self.0 |= Self::USED_MASK;
        self
    }

    #[inline]
    pub(crate) fn is_used(&self) -> bool {
        self.0 & Self::USED_MASK != 0
    }

    #[inline]
    pub(crate) fn get_new_idx(&self) -> Option<u8> {
        let remove_used_bit = self.0 & !Self::USED_MASK;
        if remove_used_bit == Self::RELOCATED {
            None
        } else {
            Some(remove_used_bit)
        }
    }

    #[inline]
    pub(crate) fn set_relocated(&mut self) -> &mut Self {
        self.0 |= Self::RELOCATED;
        self
    }

    #[allow(unused)]
    #[inline]
    pub(crate) fn set_new_idx(&mut self, idx: u8) -> &mut Self {
        assert!(idx < Self::RELOCATED);
        self.0 |= idx;
        self
    }
}

#[derive(Clone)]
pub(crate) struct GlobalDylib {
    inner: RelocatedDylib<'static>,
    flags: OpenFlags,
    deps: Option<Arc<Box<[RelocatedDylib<'static>]>>>,
    pub(crate) state: DylibState,
}

unsafe impl Send for GlobalDylib {}
unsafe impl Sync for GlobalDylib {}

impl GlobalDylib {
    #[inline]
    pub(crate) fn get_dylib(&self) -> Dylib {
        debug_assert!(self.deps.is_some());
        Dylib {
            inner: self.inner.clone(),
            flags: self.flags,
            deps: self.deps.clone(),
        }
    }

    #[inline]
    pub(crate) fn set_flags(&mut self, flags: OpenFlags) {
        self.flags = flags;
    }

    #[inline]
    pub(crate) fn flags(&self) -> OpenFlags {
        self.flags
    }

    #[inline]
    pub(crate) fn relocated_dylib(&self) -> RelocatedDylib<'static> {
        self.inner.clone()
    }

    #[inline]
    pub(crate) fn relocated_dylib_ref(&self) -> &RelocatedDylib<'static> {
        &self.inner
    }

    #[inline]
    pub(crate) fn shortname(&self) -> &str {
        self.inner.shortname()
    }

    #[inline]
    pub(crate) fn deps(&self) -> Option<&Arc<Box<[RelocatedDylib<'static>]>>> {
        self.deps.as_ref()
    }
}

pub(crate) struct Manager {
    pub(crate) all: IndexMap<String, GlobalDylib>,
    pub(crate) global: IndexMap<String, RelocatedDylib<'static>>,
}

pub(crate) static MANAGER: Lazy<RwLock<Manager>> = Lazy::new(|| {
    RwLock::new(Manager {
        all: IndexMap::new(),
        global: IndexMap::new(),
    })
});

pub(crate) fn register(
    lib: RelocatedDylib<'static>,
    flags: OpenFlags,
    deps: Option<Arc<Box<[RelocatedDylib<'static>]>>>,
    manager: &mut Manager,
    state: DylibState,
) {
    let shortname = lib.shortname().to_owned();
    log::debug!(
        "Trying to register a library. Name: [{}] flags:[{:?}]",
        shortname,
        flags
    );
    manager.all.insert(
        shortname.to_owned(),
        GlobalDylib {
            state,
            inner: lib.clone(),
            flags,
            deps,
        },
    );
    if flags.contains(OpenFlags::RTLD_GLOBAL) {
        manager.global.insert(shortname, lib);
    }
}

#[cfg(feature = "std")]
pub(crate) fn global_find(name: &str) -> Option<*const ()> {
    log::debug!("Lazy Binding: [{}]", name);
    crate::loader::builtin::BUILTIN
        .get(name)
        .copied()
        .or_else(|| {
            MANAGER.read().global.values().find_map(|lib| unsafe {
                lib.get::<()>(name).map(|sym| {
                    log::trace!(
                        "Lazy Binding: find symbol [{}] from [{}] in global scope ",
                        name,
                        lib.name()
                    );
                    let val = sym.into_raw();
                    assert!(lib.base() != val as usize);
                    val
                })
            })
        })
}
