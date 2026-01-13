// Physical memory allocator based on BuddyAllocator.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use crate::buddy::BuddyAllocator;
use crate::memlayout::{KERNBASE, PHYSTOP};
use crate::riscv::PGSIZE;
use crate::spinlock::Mutex;

unsafe extern "C" {
    // first address after kernel.
    // defined by kernel.ld
    static mut end: [u8; 0];
}

#[global_allocator]
pub static KMEM: Kmem = Kmem(Mutex::new(BuddyAllocator::new(), "kmem"));

#[alloc_error_handler]
fn on_oom(layout: Layout) -> ! {
    panic!("alloc error: {:?}", layout)
}

pub struct Kmem(Mutex<BuddyAllocator>);

unsafe impl GlobalAlloc for Kmem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock()
            .alloc(layout)
            .map_or(ptr::null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.0.lock().dealloc(ptr, layout)
    }
}

#[allow(static_mut_refs)]
pub fn init() {
    unsafe {
        KMEM.0.lock().init(end.as_ptr() as usize, PHYSTOP).unwrap();
    }
}

const NPAGE: usize = (PHYSTOP - KERNBASE) / PGSIZE;
static PAGE_REF: Mutex<[u16; NPAGE]> = Mutex::new([0; NPAGE], "pgref");

#[inline]
fn pa2idx(pa: usize) -> usize {
    assert!((KERNBASE..PHYSTOP).contains(&pa), "pa out of range");
    assert!(pa.is_multiple_of(PGSIZE), "pa not aligned");
    (pa - KERNBASE) / PGSIZE
}

pub fn page_ref_init(pa: usize) {
    let idx = pa2idx(pa);
    let mut refs = PAGE_REF.lock();
    assert!(refs[idx] == 0, "page_ref_init: ref != 0");
    refs[idx] = 1;
}

pub fn page_ref_inc(pa: usize) {
    let idx = pa2idx(pa);
    let mut refs = PAGE_REF.lock();
    let v = refs[idx];
    assert!(v != u16::MAX, "page_ref_inc overflow");
    refs[idx] = v + 1;
}

pub fn page_ref_dec(pa: usize) -> u16 {
    let idx = pa2idx(pa);
    let mut refs = PAGE_REF.lock();
    let v = refs[idx];
    assert!(v != 0, "page_ref_dec underflow");
    refs[idx] = v - 1;
    refs[idx]
}

pub fn page_ref_get(pa: usize) -> u16 {
    let idx = pa2idx(pa);
    let refs = PAGE_REF.lock();
    refs[idx]
}

pub fn free_pages() -> usize {
    KMEM.0.lock().free_bytes() / PGSIZE
}
