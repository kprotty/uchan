#[cfg_attr(any(target_arch = "x86", target_arch = "x86_64"), path = "./faa.rs")]
#[cfg_attr(not(any(target_arch = "x86", target_arch = "x86_64")), path = "./cas.rs")]
mod queue;
pub(super) use queue::Queue;

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
};
use sptr::from_exposed_addr_mut;

// AtomicPtr<T> doesn't currently expose a fetch_add() for the address of the pointer.
// This is a quick hack to currently use fetch_add() since it generates the required atomic instruction.
#[inline(always)]
pub(self) fn fetch_add_ptr<T>(ptr: &AtomicPtr<T>, addr: usize, ordering: Ordering) -> *mut T {
    let usize_ptr = (ptr as *const AtomicPtr<T>).cast::<AtomicUsize>();
    let result = unsafe { (*usize_ptr).fetch_add(addr, ordering) };
    from_exposed_addr_mut(result)
}

pub(self) struct Slot<T> {
    value: UnsafeCell<MaybeUninit<T>>,
    ready: AtomicBool,
}

impl<T> Slot<T> {
    pub(self) const EMPTY: Self = Self {
        value: UnsafeCell::new(MaybeUninit::uninit()),
        ready: AtomicBool::new(false),
    };

    #[inline(always)]
    pub(self) unsafe fn write(&self, value: T, ordering: Ordering) {
        self.value.get().write(MaybeUninit::new(value));
        self.ready.store(true, ordering)
    }

    #[inline(always)]
    pub(self) unsafe fn read(&self, ordering: Ordering) -> Option<T> {
        self.ready
            .load(ordering)
            .then(|| self.value.get().read().assume_init())
    }
}
