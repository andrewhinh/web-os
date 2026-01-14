use alloc::boxed::Box;
use core::alloc::Layout;
use core::mem::size_of;

use crate::sys;

const PGSIZE: usize = 4096;
const STACK_ALLOC_BYTES: usize = PGSIZE * 2 + size_of::<usize>();

#[repr(C)]
struct Start {
    f: extern "C" fn(usize, usize),
    arg1: usize,
    arg2: usize,
}

extern "C" fn thread_entry(start_ptr: usize, _unused: usize) -> ! {
    let start = unsafe { Box::from_raw(start_ptr as *mut Start) };
    (start.f)(start.arg1, start.arg2);
    sys::exit(0)
}

fn alloc_stack_page() -> sys::Result<usize> {
    // Allocate an aligned stack base for clone().
    let layout = Layout::from_size_align(STACK_ALLOC_BYTES, size_of::<usize>()).unwrap();
    let raw = unsafe { alloc::alloc::alloc(layout) };
    if raw.is_null() {
        return Err(sys::Error::OutOfMemory);
    }
    let mut aligned = raw as usize + size_of::<usize>();
    aligned = (aligned + PGSIZE - 1) & !(PGSIZE - 1);
    unsafe {
        *((aligned - size_of::<usize>()) as *mut usize) = raw as usize;
    }
    Ok(aligned)
}

fn free_stack_page(aligned: usize) {
    if aligned == 0 {
        return;
    }
    let layout = Layout::from_size_align(STACK_ALLOC_BYTES, size_of::<usize>()).unwrap();
    unsafe {
        let raw = *((aligned - size_of::<usize>()) as *const usize) as *mut u8;
        alloc::alloc::dealloc(raw, layout);
    }
}

pub fn thread_create(
    f: extern "C" fn(usize, usize),
    arg1: usize,
    arg2: usize,
) -> sys::Result<usize> {
    let start = Box::new(Start { f, arg1, arg2 });
    let start_ptr = Box::into_raw(start) as usize;
    let stack = alloc_stack_page()?;
    sys::clone(thread_entry as usize, start_ptr, 0, stack)
}

pub fn thread_join() -> sys::Result<usize> {
    let mut stack: usize = 0;
    let pid = sys::join(&mut stack)?;
    free_stack_page(stack);
    Ok(pid)
}
