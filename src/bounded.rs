use sptr::{write, Strict, NonNull};
use std::{
    time::Duration,
    pin::Pin,
    marker::PhantomPinned,
    alloc::{alloc, dealloc, Layout},
    mem::{drop, size_of, align_of, MaybeUninit},
    cell::{Cell, UnsafeCell},
    num::{NonZeroUsize, NonZeroU32},
    sync::atomic::{AtomicU32, AtomicPtr, Ordering},
};

type Stamp = AtomicU32;
type Value<T> = UnsafeCell<MaybeUninit<T>>;

struct Array<T> {
    ptr: NonNull<Value<T>>,
    cap: NonZeroUsize,
}

impl<T> Array<T> {
    fn stamp_offset(capacity: NonZeroUsize) -> usize {
        let align = align_of::<Stamp>().saturating_sub(align_of::<Value<T>>());
        (size_of::<Value<T>>() * capacity.get()) + align;
    }

    fn layout(capacity: NonZeroUsize) -> Layout {
        let align = align_of::<Value<T>>().max(align_of::<Stamp>());
        let size = stamp_offset(capacity) + (size_of::<Stamp>() * capacity.get());
        Layout::from_size_align(size, align).expect("capacity overflow")
    }
    
    fn alloc(capacity: NonZeroUsize) -> Self {
        let capacity = NonZeroUsize::new(capacity.get() as usize).unwrap();
        let ptr = unsafe { alloc(Self::layout(capacity)) };
        assert!(!ptr.is_null(), "out of memory");

        let values = ptr.cast::<Value<T>>();
        let stamps = ptr.add(Self::stamp_offset(capacity)).cast::<Stamp>();
        
        for i in 0..capacity.get() {
            write(values.add(i), Value::new(MaybeUninit::uninit()));
            write(stamps.add(i), Stamp::new(i as u32));
        }

        Self {
            ptr: values,
            cap: capacity,
        }
    }

    unsafe fn from(ptr: *mut Value<T>, capacity: NonZeroU32) -> Self {
        Self {
            ptr: NonNull::new(ptr).unwrap(),
            cap: NonZeroUsize::new(capacity.get() as usize).unwrap(),
        }
    }

    fn slot(&self, index: u32) -> (&Stamp, &Value<T>) {
        let index = index as usize;
        assert!(index < self.capacity.get());

        let base = self.ptr.as_ptr().cast::<u8>();
        let value = base.cast::<Value<T>>().add(index);
        let stamp = base
            .add(Self::stamp_offset(self.capacity))
            .cast::<Stamp>()
            .add(index);

        (&*stamp, &*value)
    }

    unsafe fn dealloc(self) {
        let ptr = self.ptr.as_ptr().cast::<u8>();
        let layout = Self::layout(self.capacity);
        dealloc(ptr, layout)
    }
}

enum Entry<T> {
    Empty,
    Full(T),
    Disconnected(T),
}

struct Waiter<T> {
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    entry: Cell<Entry<T>>,
    event: Event,
    _pinned: PhantomPinned,
}

struct Producer {
    tail: AtomicU32,
    pending: AtomicPtr<Waiter<T>>,
}

struct Consumer<T> {
    head: Cell<u32>,
    event: Event,
    disconnected: AtomicBool,
    waiters: Cell<Option<NonNull<Waiter<T>>>>,
}

struct Shared<T> {
    array: AtomicPtr<T>,
    capacity: NonZeroU32,
}

pub struct Channel<T> {
    producer: CachePadded<Producer>,
    consumer: CachePadded<Consumer<T>>,
    shared: CachePadded<Shared<T>>,
}

impl<T> Channel<T> {
    pub fn new(capacity: NonZeroUsize) -> Self {

    }

    pub fn try_send(&self, value: T) -> Result<(), Result<T, T>> {

    }

    pub fn send(&self, mut value: T) -> Result<(), T> {
        loop {
            value = match self.try_send(value) {
                Ok(()) => return Ok(()),
                Err(Ok(v)) => v,
                Err(Err(v)) => return Err(v),
            };

            let pending = NonNull::new(self.producer.pending.load(Ordering::Acquire));
            if pending == NonNull::dangling() {
                return Err(value);
            }

            let waiter = Waiter {
                next: Cell::new(pending),
                entry: Cell::new(Entry::Full(value)),
                event: Event::new(),
                _pinned: PhantomPinned,
            };

            let pinned_waiter = unsafe { Pin::new_unchecked(&waiter) };
            if let Err(_) = self.producer.pending.compare_exchange(
                pending.as_ptr(),
                NonNull::from(&*pinned_waiter).as_ptr(),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                value = match waiter.entry.replace(Entry::Empty) {
                    Entry::Full(v) => v,
                    _ => unreachable!(),
                };

                Backoff::spin_loop_hint();
                continue;
            }
            
            self.consumer.event.set();
            assert!(waiter.event.wait(None));

            return match waiter.entry.replace(Entry::Empty) {
                Entry::Empty => Ok(()),
                Entry::Full(_) => unreachable!(),
                Entry::Disconnected(v) => Err(v),
            };
        }
    }

    pub unsafe fn try_recv(&self) -> Result<Option<T>, ()> {
        let mut waiters = self.consumer.waiters.get();
        if waiters.is_none() {
            waiters = NonNull::new(self.producer.pending.load(Ordering::Relaxed));
            assert_ne!(waiters, Some(NonNull::dangling()));

            if waiters.is_some() {
                waiters = NonNull::new(self.producer.pending.swap(null_mut(), Ordering::Acquire));
                assert_ne!(waiters, Some(NonNull::dangling()));
            }
        }

        if let Some(waiter) = waiters {
            self.consumer.waiters.set(waiter.as_ref().next.get());

        }
    }

    pub unsafe fn recv(&self) -> Result<T, ()> {
        self.recv_with(None).map(|value| value.unwrap())
    }

    pub unsafe fn recv_timeout(&self, timeout: Duration) -> Result<Option<T>, ()> {
        self.recv_with(Some(timeout))
    }

    unsafe fn recv_with(&self, timeout: Option<Duration>) -> Result<Option<T>, ()> {
        let mut timed_out = false;
        loop {
            match self.try_recv() {
                Ok(None) => {},
                Ok(Some(value)) => return Ok(Some(value)),
                Err(()) => return Err(()),
            }

            if timed_out {
                return Ok(None);
            }

            timed_out = self.consumer.event.wait(timeout);
            self.consumer.event.reset();
        }
    }

    fn disconnect(&self, is_sender: bool) {

    }
}