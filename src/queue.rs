use crate::{
    parker::Parker,
    Event, TimedEvent,
};
use alloc::boxed::Box;
use core::{
    ptr::{NonNull, null_mut},
    cell::UnsafeCell,
    mem::{drop, MaybeUninit},
    sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, Ordering},
};
use cache_padded::CachePadded;
use sptr::Strict;

const BLOCK_ALIGN: usize = 4096;
const BLOCK_SIZE: usize = 256;

#[repr(align(4096))]
struct Block<T> {
    values: [UnsafeCell<MaybeUninit<T>>; BLOCK_SIZE],
    ready: [AtomicBool; BLOCK_SIZE],
    next: AtomicPtr<Self>,
    pending: AtomicIsize,
}

impl<T> Block<T> {
    const EMPTY_VALUE: UnsafeCell<MaybeUninit<T>> = UnsafeCell::new(MaybeUninit::uninit());
    const EMPTY_READY: AtomicBool = AtomicBool::new(false);
    const EMPTY: Self = Self {
        values: [Self::EMPTY_VALUE; BLOCK_SIZE],
        ready: [Self::EMPTY_READY; BLOCK_SIZE],
        next: AtomicPtr::new(null_mut()),
        pending: AtomicIsize::new(0),
    };

    unsafe fn drop(block: *mut Self, count: isize) {
        let pending = (*block).pending.fetch_add(count, Ordering::AcqRel);
        let new_pending = pending.wrapping_add(count);

        if new_pending == 0 {
            drop(Box::from_raw(block));
        }
    }
}

struct Consumer<T> {
    head: AtomicPtr<Block<T>>,
    parker: Parker,
}

pub(super) struct Queue<T> {
    producer: CachePadded<AtomicPtr<Block<T>>>,
    consumer: CachePadded<Consumer<T>>,
}

impl<T> Queue<T> {
    pub(super) const EMPTY: Self = Self {
        producer: CachePadded::new(AtomicPtr::new(null_mut())),
        consumer: CachePadded::new(Consumer {
            head: AtomicPtr::new(null_mut()),
            parker: Parker::EMPTY,
        }),
    };

    pub(super) fn send(&self, value: T) -> Result<(), T> {
        unimplemented!("todo")
    }

    pub(super) unsafe fn try_recv(&self) -> Result<Option<T>, ()> {
        unimplemented!("todo")
    }

    pub(super) unsafe fn recv<E: Event>(&self) -> Result<T, ()> {
        unimplemented!("todo")
    }

    pub(super) unsafe fn recv_timeout<E: TimedEvent>(
        &self,
        timeout: E::Duration,
    ) -> Result<Option<T>, ()> {
        unimplemented!("todo")
    }

    pub(super) fn disconnect(&self, is_sender: bool) {
        unimplemented!("todo")
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        unimplemented!("todo")
    }
}
