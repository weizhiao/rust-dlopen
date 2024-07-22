use crate::segment::ELFRelro;
use crate::types::ExternLibrary;
use crate::{arch::*, relocate_error};
use crate::{
    builtin::BUILTIN,
    parse_err_convert,
    types::{ELFLibrary, RelocatedLibrary},
    Rela, Result, REL_BIT, REL_MASK,
};
use alloc::boxed::Box;
use alloc::format;
use elf::abi::*;

#[derive(Debug)]
pub(crate) struct ELFRelocation {
    pub(crate) pltrel: Option<&'static [Rela]>,
    pub(crate) rel: Option<&'static [Rela]>,
    pub(crate) relro: Option<ELFRelro>,
}

impl ELFLibrary {
    pub fn relocate(self, inner_libs: &[RelocatedLibrary]) -> Result<RelocatedLibrary> {
        #[derive(Debug)]
        struct Dump;
        impl ExternLibrary for Dump {
            #[cold]
            fn get_sym(&self, _name: &str) -> Option<*const ()> {
                None
            }
        }
        self.relocate_with::<Dump>(inner_libs, None)
    }

    pub fn relocate_with<T>(
        self,
        inner_libs: &[RelocatedLibrary],
        extern_lib: Option<T>,
    ) -> Result<RelocatedLibrary>
    where
        T: ExternLibrary + 'static,
    {
        let pltrel = if let Some(pltrel) = self.relocation().pltrel {
            pltrel.iter()
        } else {
            [].iter()
        };

        let rela = if let Some(rela) = self.relocation().rel {
            rela.iter()
        } else {
            [].iter()
        };

        /*
            A Represents the addend used to compute the value of the relocatable field.
            B Represents the base address at which a shared object has been loaded into memory during execution.
            S Represents the value of the symbol whose index resides in the relocation entry.
        */

        // 因为REL_IRELATIVE的存在，对glibc来说rela和pltrel的重定位是有先后顺序的
        // 不过musl中没有出现过REL_IRELATIVE的重定位类型，我想这可能是libc实现的问题？
        for rela in rela.chain(pltrel) {
            let r_type = rela.r_info as usize & REL_MASK;
            match r_type as _ {
                // REL_GOT/REL_JUMP_SLOT: S  REL_SYMBOLIC: S + A
                REL_JUMP_SLOT | REL_GOT | REL_SYMBOLIC => {
                    let r_sym = rela.r_info as usize >> REL_BIT;
                    let dynsym = unsafe { self.symtab().add(r_sym).read() };
                    let append = rela.r_addend;
                    let symbol = if dynsym.st_info >> 4 == STB_LOCAL {
                        dynsym.st_value as _
                    } else {
                        let name = self
                            .strtab()
                            .get(dynsym.st_name as usize)
                            .map_err(parse_err_convert)?;

                        let symbol = BUILTIN
                            .get(&name)
                            .copied()
                            .or_else(|| {
                                if dynsym.st_shndx != SHN_UNDEF {
                                    return Some(unsafe {
                                        self.segments()
                                            .as_mut_ptr()
                                            .add(dynsym.st_value as usize)
                                            .cast()
                                    });
                                }

                                for lib in inner_libs.iter() {
                                    if let Some(sym) = lib.get_sym(name) {
                                        return Some(sym);
                                    }
                                }

                                if let Some(lib) = extern_lib.as_ref() {
                                    return lib.get_sym(name);
                                }

                                None
                            })
                            .ok_or_else(|| {
                                relocate_error(format!("can not relocate symbol {}", name))
                            })?;
                        symbol
                    };

                    let rel_addr = unsafe {
                        self.segments()
                            .as_mut_ptr()
                            .add(rela.r_offset.checked_add_signed(append).unwrap() as usize)
                            as *mut usize
                    };

                    unsafe { rel_addr.write(symbol as usize) }
                }
                // B + A
                REL_RELATIVE => {
                    let rel_addr = unsafe {
                        self.segments().as_mut_ptr().add(rela.r_offset as usize) as *mut usize
                    };
                    unsafe { rel_addr.write(self.segments().base() + rela.r_addend as usize) }
                }
                // indirect( B + A )
                REL_IRELATIVE => {
                    let rel_addr = unsafe {
                        self.segments().as_mut_ptr().add(rela.r_offset as usize) as *mut usize
                    };
                    let ifunc: fn() -> usize = unsafe {
                        core::mem::transmute(self.segments().base() + rela.r_addend as usize)
                    };
                    unsafe { rel_addr.write(ifunc()) }
                }

                #[cfg(feature = "tls")]
                REL_DTPMOD => {
                    let rel_addr = unsafe {
                        self.segments().as_mut_ptr().add(rela.r_offset as usize) as *mut usize
                    };
                    unsafe {
                        rel_addr.write(self.tls().as_ref().unwrap().as_ref()
                            as *const crate::tls::ELFTLS
                            as usize)
                    }
                }
                _ => {
                    // REL_TPOFF：这种类型的重定位明显做不到，它是为静态模型设计的，这种方式
                    // 可以通过带偏移量的内存读取来获取TLS变量，无需使用__tls_get_addr，
                    // 即可以使用它来较快的访问那些在程序启动时就确定加载的dso中的TLS，
                    // 实现它需要对要libc做修改，因为它要使用tp来访问thread local，
                    // 而线程栈里保存的东西完全是由libc控制的

                    return Err(relocate_error(format!(
                        "unsupport relocate type {}",
                        r_type
                    )));
                }
            }
        }

        if let Some(init) = self.init_fn() {
            init();
        }

        if let Some(init_array) = self.init_array_fn() {
            for init in *init_array {
                init();
            }
        }

        if let Some(relro) = &self.relocation().relro {
            relro.relro()?;
        }

        let extern_lib = extern_lib.map(|lib| Box::new(lib) as Box<dyn ExternLibrary>);

        Ok(RelocatedLibrary::new(self, inner_libs.to_vec(), extern_lib))
    }
}
