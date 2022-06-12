use crate::{
    atomic::{Backoff, CachePadded},
    error::{RecvError, SendError, TryRecvError, TrySendError},
    parker::Parker,
};
use std::{
    alloc::{alloc, dealloc, Layout},
    cell::{Cell, UnsafeCell},
    fmt,
    marker::PhantomData,
    mem::{align_of, drop, size_of, MaybeUninit},
    slice,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::Arc,
};

struct Array<T> {
    ptr: *const u8,
    capacity: usize,
    _p: PhantomData<(T, AtomicBool)>,
}

impl<T> Array<T> {
    fn alloc(capacity: usize) -> Self {
        unsafe {
            let ptr = alloc(Self::layout(capacity));
            assert!(!ptr.is_null());

            let stored = ptr
                .add(Self::stored_offset(capacity))
                .cast::<MaybeUninit<AtomicBool>>();
            for i in 0..capacity {
                stored
                    .add(i)
                    .write(MaybeUninit::new(AtomicBool::new(false)));
            }

            Self {
                ptr: ptr as *const u8,
                capacity,
                _p: PhantomData,
            }
        }
    }

    fn stored_offset(capacity: usize) -> usize {
        let align = align_of::<AtomicBool>();
        let offset = capacity * size_of::<T>();
        (offset + align - 1) / (align * align)
    }

    fn layout(capacity: usize) -> Layout {
        Layout::from_size_align(
            Self::stored_offset(capacity) + (capacity * size_of::<AtomicBool>()),
            align_of::<T>(),
        )
        .unwrap()
    }

    fn values(&self) -> &[UnsafeCell<MaybeUninit<T>>] {
        self.slice(0)
    }

    fn stored(&self) -> &[AtomicBool] {
        self.slice(Self::stored_offset(self.capacity))
    }

    fn slice<S>(&self, offset: usize) -> &[S] {
        unsafe {
            let ptr = self.ptr.add(offset).cast();
            slice::from_raw_parts(ptr, self.capacity)
        }
    }
}

impl<T> Drop for Array<T> {
    fn drop(&mut self) {
        let ptr = self.ptr as *mut u8;
        let layout = Self::layout(self.capacity);
        unsafe { dealloc(ptr, layout) }
    }
}

struct Producer {
    tail: AtomicUsize,
    sema: AtomicUsize,
}

struct Consumer<T> {
    head: Cell<usize>,
    array: Array<T>,
}

struct Queue<T> {
    producer: CachePadded<Producer>,
    consumer: CachePadded<Consumer<T>>,
}

impl<T> Queue<T> {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two();
        Self {
            producer: CachePadded(Producer {
                tail: AtomicUsize::new(0),
                sema: AtomicUsize::new(capacity),
            }),
            consumer: CachePadded(Consumer {
                head: Cell::new(0),
                array: Array::alloc(capacity),
            }),
        }
    }

    fn can_push(&self) -> bool {
        self.producer.sema.load(Ordering::Relaxed) > 0
    }

    fn try_push(&self, value: T) -> Result<(), T> {
        let mut backoff = Backoff::default();
        loop {
            let sema = self.producer.sema.load(Ordering::Relaxed);
            let new_sema = match sema.checked_sub(1) {
                Some(new_sema) => new_sema,
                None => return Err(value),
            };

            if let Err(_) = self.producer.sema.compare_exchange_weak(
                sema,
                new_sema,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                backoff.yield_now();
                continue;
            }

            return Ok(unsafe {
                let tail = self.producer.tail.fetch_add(1, Ordering::Relaxed);
                let slot = tail & (self.consumer.array.capacity - 1);

                let array = &self.consumer.array;
                array
                    .values()
                    .get_unchecked(slot)
                    .get()
                    .write(MaybeUninit::new(value));
                array
                    .stored()
                    .get_unchecked(slot)
                    .store(true, Ordering::Release);
            });
        }
    }

    unsafe fn can_pop(&self) -> bool {
        let head = self.consumer.head.get();
        let slot = head & (self.consumer.array.capacity - 1);

        let stored = self.consumer.array.stored().get_unchecked(slot);
        stored.load(Ordering::Relaxed)
    }

    unsafe fn try_pop(&self) -> Option<T> {
        let head = self.consumer.head.get();
        let slot = head & (self.consumer.array.capacity - 1);

        let stored = self.consumer.array.stored().get_unchecked(slot);
        if !stored.load(Ordering::Acquire) {
            return None;
        }

        let value = self.consumer.array.values().get_unchecked(slot);
        let value = value.get().read().assume_init();
        stored.store(false, Ordering::Release);

        self.producer.sema.fetch_add(1, Ordering::Release);
        self.consumer.head.set(head.wrapping_add(1));
        Some(value)
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        unsafe {
            while let Some(value) = self.try_pop() {
                drop(value);
            }
        }
    }
}

struct Channel<T> {
    queue: Queue<T>,
    senders: Parker,
    receiver: Parker,
    disconnected: AtomicBool,
}

impl<T> fmt::Debug for Channel<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel").finish()
    }
}

pub fn new<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let channel = Arc::new(Channel {
        queue: Queue::new(capacity),
        senders: Parker::default(),
        receiver: Parker::default(),
        disconnected: AtomicBool::new(false),
    });

    let sender = Sender(channel.clone());
    let receiver = Receiver {
        channel,
        _marker: PhantomData,
    };

    (sender, receiver)
}

#[derive(Debug)]
pub struct Sender<T>(Arc<Channel<T>>);

impl<T> Sender<T> {
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        if self.0.disconnected.load(Ordering::Relaxed) {
            return Err(TrySendError::Disconnected(value));
        }

        if let Err(value) = self.0.queue.try_push(value) {
            return Err(TrySendError::Full(value));
        }

        self.0.receiver.unpark(1);
        Ok(())
    }

    pub async fn send(&self, mut value: T) -> Result<(), SendError<T>> {
        loop {
            value = match self.try_send(value) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(value)) => value,
                Err(TrySendError::Disconnected(value)) => return Err(SendError(value)),
            };

            let should_park = || {
                let disconnected = self.0.disconnected.load(Ordering::Relaxed);
                disconnected || self.0.queue.can_push()
            };

            self.0.senders.park(should_park).await;
        }
    }

    pub fn blocking_send(&self, value: T) -> Result<(), SendError<T>> {
        unsafe { Parker::block_on(self.send(value)) }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 2 {
            self.0.disconnected.store(true, Ordering::Relaxed);
            self.0.receiver.unpark(1);
        }
    }
}

#[derive(Debug)]
pub struct Receiver<T> {
    channel: Arc<Channel<T>>,
    _marker: PhantomData<*mut ()>,
}

impl<T> Receiver<T> {
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        if let Some(value) = unsafe { self.channel.queue.try_pop() } {
            self.channel.senders.unpark(1);
            return Ok(value);
        }

        if self.channel.disconnected.load(Ordering::Relaxed) {
            return Err(TryRecvError::Disconnected);
        }

        Err(TryRecvError::Empty)
    }

    pub async fn recv(&mut self) -> Result<T, RecvError> {
        loop {
            match self.try_recv() {
                Ok(value) => return Ok(value),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }

            let should_park = || {
                let disconnected = self.channel.disconnected.load(Ordering::Relaxed);
                disconnected || unsafe { self.channel.queue.can_pop() }
            };

            self.channel.receiver.park(should_park).await;
        }
    }

    pub fn blocking_recv(&mut self) -> Result<T, RecvError> {
        unsafe { Parker::block_on(self.recv()) }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.channel.disconnected.store(true, Ordering::Relaxed);
        self.channel.senders.unpark(usize::MAX);
    }
}
