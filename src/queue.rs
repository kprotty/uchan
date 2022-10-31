use crate::{
    backoff::Backoff,
    event::{Event, TimedEvent},
    parker::{Parker, TryParkResult},
};
use alloc::boxed::Box;
use cache_padded::CachePadded;
use core::{
    cell::UnsafeCell,
    mem::{drop, MaybeUninit},
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicUsize, Ordering},
};
use sptr::{from_exposed_addr_mut, Strict};

const BLOCK_ALIGN: usize = 4096;
const BLOCK_SIZE: usize = 256;

#[repr(align(4096))]
struct Block<T> {
    values: [UnsafeCell<MaybeUninit<T>>; BLOCK_SIZE],
    stored: [AtomicBool; BLOCK_SIZE],
    next: AtomicPtr<Self>,
    pending: AtomicIsize,
}

const INDEX_SHIFT: u32 = 1;
const DISCONNECT_BIT: usize = 0b1;

impl<T> Block<T> {
    const EMPTY_VALUE: UnsafeCell<MaybeUninit<T>> = UnsafeCell::new(MaybeUninit::uninit());
    const EMPTY_STORED: AtomicBool = AtomicBool::new(false);
    const EMPTY: Self = Self {
        values: [Self::EMPTY_VALUE; BLOCK_SIZE],
        stored: [Self::EMPTY_STORED; BLOCK_SIZE],
        next: AtomicPtr::new(null_mut()),
        pending: AtomicIsize::new(0),
    };

    #[inline(always)]
    fn fetch_add(block_ptr: &AtomicPtr<Self>, ptr: usize, ordering: Ordering) -> *mut Self {
        let usize_ptr = (block_ptr as *const AtomicPtr<Self>).cast::<AtomicUsize>();
        let result = unsafe { (*usize_ptr).fetch_add(ptr, ordering) };
        from_exposed_addr_mut(result)
    }

    fn encode(block: *mut Self, index: usize, disconnected: bool) -> *mut Self {
        block.map_addr(|addr| {
            assert_eq!(addr & (BLOCK_ALIGN - 1), 0);
            assert!(index <= (BLOCK_ALIGN - 1) >> INDEX_SHIFT);
            addr | (index << INDEX_SHIFT) | (disconnected as usize)
        })
    }

    fn decode(ptr: *mut Self) -> (*mut Self, usize, bool) {
        let block = ptr.map_addr(|addr| addr & !(BLOCK_ALIGN - 1));
        let index = (ptr.addr() & (BLOCK_ALIGN - 1)) >> INDEX_SHIFT;
        let disconnected = ptr.addr() & DISCONNECT_BIT != 0;
        (block, index, disconnected)
    }

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
    disconnected: AtomicBool,
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
            disconnected: AtomicBool::new(false),
        }),
    };

    pub(super) fn send(&self, value: T) -> Result<(), T> {
        unsafe {
            let mut new_block: *mut Block<T> = null_mut();

            let result = (|| loop {
                let one_index = 1 << INDEX_SHIFT;
                let tail = Block::<T>::fetch_add(&*self.producer, one_index, Ordering::Acquire);

                let (block, index, disconnected) = Block::<T>::decode(tail);
                if disconnected {
                    return Err(());
                }

                if !block.is_null() && index < BLOCK_SIZE {
                    return Ok((block, index, None));
                }

                let prev_link = match NonNull::new(block) {
                    Some(prev_block) => NonNull::from(&prev_block.as_ref().next),
                    None => NonNull::from(&self.consumer.head),
                };

                let mut next_block = prev_link.as_ref().load(Ordering::Acquire);
                if next_block.is_null() {
                    if new_block.is_null() {
                        new_block = Box::into_raw(Box::new(Block::EMPTY));
                        assert!(!new_block.is_null());
                    }

                    next_block = new_block;
                    match prev_link.as_ref().compare_exchange(
                        null_mut(),
                        next_block,
                        Ordering::Release,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => new_block = null_mut(),
                        Err(e) => next_block = e,
                    }
                }

                loop {
                    let tail = self.producer.load(Ordering::Acquire);

                    let (current_block, current_index, disconnected) = Block::<T>::decode(tail);
                    if disconnected {
                        return Err(());
                    }

                    if current_block != block {
                        if !block.is_null() {
                            Block::drop(block, 1);
                        }

                        Backoff::spin_loop_hint();
                        break;
                    }

                    if let Err(_) = self.producer.compare_exchange_weak(
                        tail,
                        Block::encode(next_block, 1, false),
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Backoff::yield_now();
                        continue;
                    }

                    let (pending_block, count) = match NonNull::new(block) {
                        Some(_) => (block, (current_index as usize) - BLOCK_SIZE),
                        None => (next_block, 1),
                    };

                    let pending = Some((pending_block, count));
                    return Ok((next_block, 0, pending));
                }
            })();

            let result = match result {
                Err(()) => Err(value),
                Ok((block, index, pending)) => Ok({
                    (*block).values[index].get().write(MaybeUninit::new(value));
                    (*block).stored[index].store(true, Ordering::SeqCst);
                    self.consumer.parker.unpark();

                    if let Some((pending_block, count)) = pending {
                        Block::drop(pending_block, count.try_into().unwrap());
                    }
                }),
            };

            if !new_block.is_null() {
                drop(Box::from_raw(new_block));
            }

            result
        }
    }

    pub(super) unsafe fn try_recv(&self) -> Result<Option<T>, ()> {
        let head = self.consumer.head.load(Ordering::Acquire);
        let (mut block, mut index, _) = Block::<T>::decode(head);

        if !block.is_null() {
            if index == BLOCK_SIZE {
                let next_block = (*block).next.load(Ordering::Acquire);
                if !next_block.is_null() {
                    Block::drop(block, 1);

                    block = next_block;
                    index = 0;

                    let new_head = Block::encode(block, index, false);
                    self.consumer.head.store(new_head, Ordering::Relaxed);
                }
            }

            if index < BLOCK_SIZE {
                if (*block).stored[index].load(Ordering::Acquire) {
                    let new_head = Block::encode(block, index + 1, false);
                    self.consumer.head.store(new_head, Ordering::Relaxed);

                    let value = (*block).values[index].get().read().assume_init();
                    return Ok(Some(value));
                }
            }
        }

        if self.consumer.disconnected.load(Ordering::Acquire) {
            Err(())
        } else {
            Ok(None)
        }
    }

    pub(super) unsafe fn recv<E: Event>(&self) -> Result<T, ()> {
        let mut spins: u32 = 16;
        loop {
            match self.try_recv() {
                Ok(None) => {}
                Err(()) => return Err(()),
                Ok(Some(value)) => return Ok(value),
            }

            spins = spins.saturating_sub(1);
            match spins {
                0 => self.consumer.parker.park::<E>(),
                _ => Backoff::spin_loop_hint(),
            }
        }
    }

    pub(super) unsafe fn recv_timeout<E: TimedEvent>(
        &self,
        timeout: E::Duration,
    ) -> Result<Option<T>, ()> {
        let mut timeout = Some(timeout);
        loop {
            match self.try_recv() {
                Ok(None) if timeout.is_some() => {}
                result => return result,
            }

            let duration = timeout.take().unwrap();
            match self.consumer.parker.try_park_for::<E>(duration) {
                TryParkResult::Interrupted(unused) => timeout = Some(unused),
                TryParkResult::Unparked => {}
                TryParkResult::Timeout => {}
            }
        }
    }

    pub(super) fn disconnect(&self, is_sender: bool) {
        if is_sender {
            self.consumer.disconnected.store(true, Ordering::Release);
            self.consumer.parker.unpark();
            return;
        }

        let tail = Block::<T>::fetch_add(&*self.producer, DISCONNECT_BIT, Ordering::Release);
        let (_, _, disconnected) = Block::<T>::decode(tail);
        assert!(!disconnected);
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        let head = self.consumer.head.load(Ordering::Acquire);
        let (mut block, mut index, _) = Block::<T>::decode(head);

        while !block.is_null() {
            unsafe {
                for i in index..BLOCK_SIZE {
                    match (*block).stored[i].load(Ordering::Acquire) {
                        true => drop((*block).values[i].get().read().assume_init()),
                        _ => break,
                    }
                }

                let next_block = (*block).next.load(Ordering::Acquire);
                drop(Box::from_raw(block));

                block = next_block;
                index = 0;
            }
        }
    }
}
