mod loader;
mod register;
mod traits;
mod types;

pub use loader::ElfLibrary;
pub use traits::AsFilename;

pub(crate) use loader::{DylibExt, ElfDylib, LoadedDylib, find_symbol, new_loader};
#[cfg(not(feature = "std"))]
pub(crate) use loader::{RuntimeLoader, shortname_from_name};
pub(crate) use register::{
    GlobalMeta, LibraryLookup, MANAGER, Manager, addr2dso, global_find, next_find, register_loaded,
    register_pending,
};
pub(crate) use types::{ARGC, ARGV, ENVP, ExtraData, FileIdentity, LinkMap};
