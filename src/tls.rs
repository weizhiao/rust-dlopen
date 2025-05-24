use alloc::{
    alloc::{dealloc, handle_alloc_error},
    boxed::Box,
    vec::Vec,
};
use core::{
    alloc::Layout,
    ffi::{c_int, c_void},
    mem::ManuallyDrop,
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
};
use elf_loader::{UserData, arch::ElfPhdr, segment::ElfSegments};
use spin::Lazy;
use thread_register::{ModifyRegister, ThreadRegister};

#[repr(C)]
pub(crate) struct TlsIndex {
    ti_module: usize,
    ti_offset: usize,
}

pub(crate) const DTV_OFFSET: usize = 8;
const TLS_TCB_SIZE: usize = 2368;
pub(crate) const TLS_INFO_ID: u8 = 2;

// struct StaticTlsInfo {
//     size: usize,
//     align: usize,
//     nelem: usize,
// }

// static STATIC_TLS_INFO: Once<StaticTlsInfo> = Once::new();
pub(crate) static mut TLS_STATIC_SIZE: usize = 0;
pub(crate) static mut TLS_STATIC_ALIGN: usize = 0;
static TLS_NEXT_DTV_IDX: AtomicUsize = AtomicUsize::new(1);

pub(crate) static TLS_GENERATION: AtomicUsize = AtomicUsize::new(0);
static HAS_SLOT_GAPS: AtomicBool = AtomicBool::new(false);
static DTV_SLOT_LIST: Lazy<DtvSlotList> = Lazy::new(|| DtvSlotList::new());

const SLOT_SIZE: usize = 20;

pub(crate) struct TlsInfo {
    image: &'static [u8],
    pub(crate) modid: usize,
    pub(crate) static_tls_offset: Option<usize>,
    memsz: usize,
    align: usize,
}

struct DtvSlot {
    generation: AtomicUsize,
    tls_info: AtomicPtr<TlsInfo>,
}

impl DtvSlot {
    fn tls_info(&self) -> *const TlsInfo {
        self.tls_info.load(Ordering::Relaxed)
    }
}

impl Default for DtvSlot {
    fn default() -> Self {
        Self {
            generation: Default::default(),
            tls_info: AtomicPtr::new(null_mut()),
        }
    }
}

struct DtvSlotList {
    next: AtomicPtr<DtvSlotList>,
    slots: [DtvSlot; SLOT_SIZE],
}

impl DtvSlotList {
    fn new() -> Self {
        DtvSlotList {
            next: AtomicPtr::new(null_mut()),
            slots: core::array::from_fn(|_| DtvSlot::default()),
        }
    }

    fn add_slot(&self, tls_info: *const TlsInfo) {
        let info = unsafe { &*tls_info };
        let mut idx = info.modid;
        let mut prev: *const DtvSlotList;
        let mut cur = self;
        loop {
            if idx < SLOT_SIZE {
                break;
            }
            idx -= SLOT_SIZE;
            prev = cur;
            let next = cur.next.load(Ordering::Relaxed);
            if next.is_null() {
                assert!(idx == 0);
                let new_slot = Box::leak(Box::new(DtvSlotList::new()));
                let ptr = new_slot as *mut DtvSlotList;
                cur = new_slot;
                (unsafe { &*prev }).next.store(ptr, Ordering::Release);
                break;
            }
            cur = unsafe { &mut *next };
        }
        cur.slots[idx]
            .generation
            .store(TLS_GENERATION.load(Ordering::Relaxed), Ordering::Relaxed);
        cur.slots[idx]
            .tls_info
            .store(tls_info as *mut TlsInfo, Ordering::Relaxed);
    }

    fn find_slot(&self, mut idx: usize) -> &DtvSlot {
        let mut node = self;
        loop {
            if idx < SLOT_SIZE {
                break;
            }
            idx -= SLOT_SIZE;
            node = unsafe { &*node.next.load(Ordering::Relaxed) };
        }
        &node.slots[idx]
    }

    fn find_free_slot(&self) -> Option<(usize, &DtvSlot)> {
        let mut node = self;
        let mut result = 0;
        loop {
            for slot in node.slots.iter() {
                if slot.tls_info.load(Ordering::Relaxed).is_null() {
                    return Some((result, slot));
                }
                result += 1;
            }
            let next = node.next.load(Ordering::Relaxed);
            if next.is_null() {
                return None;
            }
            node = unsafe { &mut *next };
        }
    }

    fn update_slotinfo(&self, dtv: &mut DtvHeader, req_modid: usize) -> *const TlsInfo {
        let slot = self.find_slot(req_modid);
        let dtv_gen = dtv.get_gen();
        let new_gen = slot.generation.load(Ordering::Relaxed);
        let mut tls_info = null();
        if dtv_gen != new_gen {
            let max_modid = TLS_NEXT_DTV_IDX.load(Ordering::Relaxed) - 1;
            let mut cur_node = self;
            let mut total = 0;
            loop {
                for (cnt, slot) in cur_node.slots.iter().enumerate() {
                    let cur_modid = cnt + if total == 0 { 1 } else { total };
                    if cur_modid > max_modid {
                        break;
                    }
                    let cur_gen = slot.generation.load(Ordering::Relaxed);
                    if cur_gen > new_gen && cur_gen <= dtv_gen {
                        continue;
                    }
                    let cur_tls_info = slot.tls_info.load(Ordering::Relaxed);
                    if dtv.dtv_cnt() < cur_modid + 1 {
                        if cur_tls_info.is_null() {
                            continue;
                        }
                        dtv.resize(max_modid);
                    }
                    if cur_modid == req_modid {
                        tls_info = cur_tls_info;
                    }
                    dtv.try_free_dtv_entry(cur_modid);
                }
                total += SLOT_SIZE;
                if total >= max_modid {
                    break;
                }
                if let Some(node) = cur_node.next_node() {
                    cur_node = node;
                } else {
                    break;
                }
            }
            dtv.set_gen(new_gen);
        }
        return tls_info;
    }

    fn next_node(&self) -> Option<&mut DtvSlotList> {
        let next = self.next.load(Ordering::Relaxed);
        if next.is_null() {
            return None;
        }
        Some(unsafe { &mut *next })
    }
}

struct DtvPointer {
    ptr: *mut u8,
    layout: Option<Layout>,
}

pub(crate) union DtvElem {
    ptr: ManuallyDrop<DtvPointer>,
    generation: usize,
}

impl Default for DtvElem {
    fn default() -> Self {
        Self {
            ptr: ManuallyDrop::new(DtvPointer {
                ptr: null_mut(),
                layout: None,
            }),
        }
    }
}

impl DtvElem {
    fn new_dynamic(tls_info: &TlsInfo) -> Self {
        let layout = Layout::from_size_align(tls_info.memsz, tls_info.align).unwrap();
        let ptr = unsafe { alloc::alloc::alloc(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, tls_info.memsz) };
        let filesz = tls_info.image.len();
        slice[..filesz].copy_from_slice(tls_info.image);
        slice[filesz..].fill(0);
        Self {
            ptr: ManuallyDrop::new(DtvPointer {
                ptr,
                layout: Some(layout),
            }),
        }
    }

    fn new_static(tls_info: &TlsInfo, dest: *mut u8) -> Self {
        let slice = unsafe { core::slice::from_raw_parts_mut(dest as *mut u8, tls_info.memsz) };
        let filesz = tls_info.image.len();
        slice[..filesz].copy_from_slice(tls_info.image);
        slice[filesz..].fill(0);
        Self {
            ptr: ManuallyDrop::new(DtvPointer {
                ptr: dest as _,
                layout: None,
            }),
        }
    }

    fn dtv_ptr(&self) -> *const u8 {
        return unsafe { self.ptr.ptr };
    }
}

struct DtvHeader {
    dtv: Vec<DtvElem>,
}

impl DtvHeader {
    fn new() -> Self {
        let mut dtv = Vec::new();
        dtv.push(DtvElem { generation: 0 });
        Self { dtv }
    }

    fn with_capicity(capacity: usize) -> Self {
        let mut dtv: Vec<DtvElem> = Vec::with_capacity(capacity);
        dtv.push(DtvElem { generation: 0 });
        Self { dtv }
    }

    fn set_dtv_header(dtv: &DtvHeader) {
        unsafe {
            ThreadRegister::set(DTV_OFFSET, dtv as *const DtvHeader as usize);
        }
    }

    fn get_dtv_header() -> &'static mut DtvHeader {
        let dtv = unsafe { ThreadRegister::get(DTV_OFFSET) };
        dtv
    }

    fn get_gen(&self) -> usize {
        unsafe { self.dtv[0].generation }
    }

    fn set_gen(&mut self, generation: usize) {
        self.dtv[0].generation = generation;
    }

    fn dtv_cnt(&self) -> usize {
        self.dtv.len() - 1
    }

    fn try_free_dtv_entry(&mut self, modid: usize) {
        if modid >= self.dtv.len() {
            return;
        }
        let entry = &mut self.dtv[modid];
        let ptr = unsafe { entry.ptr.ptr };
        if ptr.is_null() {
            return;
        }
        if let Some(layout) = unsafe { entry.ptr.layout } {
            unsafe {
                dealloc(ptr, layout);
                entry.ptr.ptr = null_mut();
            }
        }
    }

    fn resize(&mut self, max_modid: usize) {
        assert!(max_modid + 2 > self.dtv.len());
        self.dtv.resize_with(max_modid + 2, || DtvElem::default());
    }
}

fn get_slot_list() -> &'static DtvSlotList {
    &DTV_SLOT_LIST
}

pub(crate) fn update_generation() {
    TLS_GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub(crate) enum TlsState {
    Dynamic,
    Static,
    Initialized(usize),
}

pub(crate) fn add_tls(
    segments: &ElfSegments,
    phdr: &ElfPhdr,
    data: &mut UserData,
    state: TlsState,
) {
    let memsz = phdr.p_memsz as usize;
    if memsz == 0 {
        return;
    }
    let image = unsafe {
        core::slice::from_raw_parts(
            (segments.base() + phdr.p_vaddr as usize) as *const u8,
            phdr.p_filesz as usize,
        )
    };
    let align = phdr.p_align as usize;
    let list = get_slot_list();
    let static_tls_offset = match state {
        TlsState::Dynamic => None,
        TlsState::Static => {
            let mut tls_offset = unsafe { TLS_STATIC_SIZE };
            tls_offset += memsz + align - 1;
            tls_offset -= (tls_offset + phdr.p_vaddr as usize) & (align - 1);
            unsafe { TLS_STATIC_SIZE = tls_offset };
            unsafe {
                TLS_STATIC_ALIGN = TLS_STATIC_ALIGN.max(align);
            }
            Some(tls_offset)
        }
        TlsState::Initialized(static_tls_offset) => Some(static_tls_offset),
    };
    if HAS_SLOT_GAPS.load(Ordering::Relaxed) {
        if let Some((modid, slot)) = list.find_free_slot() {
            let tls_info = Box::new(TlsInfo {
                image,
                modid,
                memsz,
                align,
                static_tls_offset,
            });
            let ptr = tls_info.as_ref() as *const _ as _;
            slot.tls_info.store(ptr, Ordering::Release);
            data.insert(TLS_INFO_ID, tls_info);
            return;
        }
        HAS_SLOT_GAPS.store(false, Ordering::Relaxed);
    }
    let modid = TLS_NEXT_DTV_IDX.fetch_add(1, Ordering::Relaxed);
    let tls_info = Box::new(TlsInfo {
        image,
        modid,
        memsz,
        align,
        static_tls_offset,
    });
    let ptr = tls_info.as_ref() as *const _ as _;
    list.add_slot(ptr);
    data.insert(TLS_INFO_ID, tls_info);
}

fn tls_get_addr_tail(
    tls_index: &TlsIndex,
    header: &mut DtvHeader,
    tls_info: *const TlsInfo,
) -> *const u8 {
    let tls_info = if tls_info.is_null() {
        let slot_list = get_slot_list();
        unsafe { &*slot_list.find_slot(tls_index.ti_module).tls_info() }
    } else {
        unsafe { &*tls_info }
    };
    let new_entry = DtvElem::new_dynamic(tls_info);
    let ptr = new_entry.dtv_ptr();
    header.dtv[tls_index.ti_module] = new_entry;
    unsafe { ptr.add(tls_index.ti_offset) }
}

fn update_get_addr(tls_index: &TlsIndex, header: &mut DtvHeader) -> *const u8 {
    let tls_info = get_slot_list().update_slotinfo(header, tls_index.ti_module);
    let ptr = header.dtv[tls_index.ti_module].dtv_ptr();
    if ptr.is_null() {
        return tls_get_addr_tail(tls_index, header, tls_info);
    }
    unsafe { ptr.add(tls_index.ti_offset) }
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn __tls_get_addr(tls_index: &TlsIndex) -> *const u8 {
    let header = DtvHeader::get_dtv_header();
    let generation = TLS_GENERATION.load(Ordering::Relaxed);
    if header.get_gen() != generation {
        return update_get_addr(tls_index, header);
    }
    let ptr = header.dtv[tls_index.ti_module].dtv_ptr();
    if ptr.is_null() {
        return tls_get_addr_tail(tls_index, header, null());
    }
    return unsafe { ptr.add(tls_index.ti_offset) };
}

pub(crate) fn init_tls() {
    let header = Box::leak(Box::new(DtvHeader::new()));
    DtvHeader::set_dtv_header(header);
}

fn get_header_from_tcb(tcb: *mut u8) -> *mut *mut DtvHeader {
    unsafe { tcb.add(DTV_OFFSET).cast::<*mut DtvHeader>() }
}

fn allocate_dtv(tcb: *mut u8) -> *mut u8 {
    let len = TLS_NEXT_DTV_IDX.load(Ordering::Relaxed);
    unsafe { get_header_from_tcb(tcb).write(Box::leak(Box::new(DtvHeader::with_capicity(len)))) };
    tcb
}

fn allocate_tls_storage() -> *mut u8 {
    let size = unsafe { TLS_STATIC_SIZE };
    let align = unsafe { TLS_STATIC_ALIGN };
    let layout = Layout::from_size_align(size, align).unwrap();
    let allocated = unsafe { alloc::alloc::alloc(layout) };
    if allocated.is_null() {
        handle_alloc_error(layout);
    }
    let tcb = unsafe { allocated.add(size - TLS_TCB_SIZE) };
    unsafe { core::slice::from_raw_parts_mut(tcb, TLS_TCB_SIZE).fill(0) };
    allocate_dtv(tcb)
}

fn init_tls_storage(tcb: *mut u8) -> *const c_void {
    if tcb.is_null() {
        return null();
    }
    let header = unsafe { &mut **get_header_from_tcb(tcb) };
    let count = TLS_NEXT_DTV_IDX.load(Ordering::Relaxed);
    if header.dtv.len() < count {
        header.resize(count);
    }
    let mut cur_node = get_slot_list();
    let max_modid = count - 1;
    let mut max_gen = 0;
    let mut total = 0;
    loop {
        for (cnt, slot) in cur_node.slots.iter().enumerate() {
            let cur_modid = cnt + total;
            if cur_modid == max_modid {
                break;
            }
            let cur_tls_info = slot.tls_info.load(Ordering::Relaxed);
            if cur_tls_info.is_null() {
                continue;
            }
            let cur_tls_info = unsafe { &*cur_tls_info };
            max_gen = max_gen.max(slot.generation.load(Ordering::Relaxed));
            if let Some(static_tls_offset) = cur_tls_info.static_tls_offset {
                let dest = unsafe { tcb.sub(static_tls_offset) };
                header.dtv[cur_tls_info.modid] = DtvElem::new_static(cur_tls_info, dest);
            }
        }
        total += SLOT_SIZE;
        if total >= max_modid {
            break;
        }
        if let Some(node) = cur_node.next_node() {
            cur_node = node;
        } else {
            break;
        }
    }
    header.set_gen(max_gen);
    tcb as _
}

#[unsafe(no_mangle)]
extern "C" fn _dl_allocate_tls(mem: *const c_void) -> *const c_void {
    let tcb = if mem.is_null() {
        allocate_tls_storage()
    } else {
        allocate_dtv(mem as _)
    };
    init_tls_storage(tcb)
}

#[unsafe(no_mangle)]
// FIXME: 有内存泄漏
extern "C" fn __cxa_thread_atexit_impl() -> c_int {
    0
}
