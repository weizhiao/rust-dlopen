use core::ptr;

const TCB_SELF_OFFSET: usize = 0x00;
const TCB_DTV_OFFSET: usize = 0x08;
const TCB_THREAD_SELF_OFFSET: usize = 0x10;
const TCB_STACK_GUARD_OFFSET: usize = 0x28;
const TCB_POINTER_GUARD_OFFSET: usize = 0x30;

const BOOTSTRAP_STACK_GUARD: usize = 0x2f6a_5d1b_3c4e_8790;
const BOOTSTRAP_POINTER_GUARD: usize = 0x6b43_1d29_84a0_7c5e;

pub(crate) unsafe fn init_initial_thread_control_block(tp: *mut u8) {
    unsafe {
        ptr::write(tp.add(TCB_SELF_OFFSET) as *mut *mut u8, tp);
        ptr::write(tp.add(TCB_DTV_OFFSET) as *mut *mut u8, ptr::null_mut());
        ptr::write(tp.add(TCB_THREAD_SELF_OFFSET) as *mut *mut u8, tp);
        ptr::write(
            tp.add(TCB_STACK_GUARD_OFFSET) as *mut usize,
            BOOTSTRAP_STACK_GUARD,
        );
        ptr::write(
            tp.add(TCB_POINTER_GUARD_OFFSET) as *mut usize,
            BOOTSTRAP_POINTER_GUARD,
        );
    }
}
