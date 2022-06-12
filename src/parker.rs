use std::{
    cell::{Cell, UnsafeCell},
    future::Future,
    marker::PhantomPinned,
    mem::drop,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{fence, AtomicBool, AtomicU8, AtomicUsize, Ordering},
    sync::Mutex,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    thread,
};

const EMPTY: u8 = 0;
const WAITING: u8 = 1;
const UPDATING: u8 = 2;
const NOTIFIED: u8 = 3;

#[derive(Default)]
struct AtomicWaker {
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
}

impl AtomicWaker {
    fn poll(&self, waker: &Waker) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        if state == NOTIFIED {
            return Poll::Ready(());
        }

        assert!(state == EMPTY || state == WAITING);
        if let Err(state) =
            self.state
                .compare_exchange(state, UPDATING, Ordering::Acquire, Ordering::Acquire)
        {
            assert_eq!(state, NOTIFIED);
            return Poll::Ready(());
        }

        unsafe {
            let w = &mut *self.waker.get();
            if !w.as_ref().map(|w| w.will_wake(waker)).unwrap_or(false) {
                *w = Some(waker.clone());
            }
        }

        match self
            .state
            .compare_exchange(UPDATING, WAITING, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Poll::Pending,
            Err(NOTIFIED) => Poll::Ready(()),
            Err(_) => unreachable!(),
        }
    }

    fn wake(&self) {
        let state = self.state.swap(NOTIFIED, Ordering::AcqRel);
        if state != WAITING {
            return;
        }

        let waker = unsafe { (*self.waker.get()).take() };
        waker.map(Waker::wake).unwrap_or(())
    }
}

#[derive(Default)]
struct Waiter {
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    waiting: Cell<bool>,
    waker: AtomicWaker,
    _pin: PhantomPinned,
}

#[derive(Default)]
struct WaitQueue {
    head: Option<NonNull<Waiter>>,
    tail: Option<NonNull<Waiter>>,
}

impl WaitQueue {
    unsafe fn push(&mut self, waiter: Pin<&Waiter>) {
        let waiter = NonNull::from(&*waiter);
        waiter.as_ref().waiting.set(true);
        waiter.as_ref().next.set(None);

        let tail = self.tail.replace(waiter);
        waiter.as_ref().prev.set(tail);

        match tail {
            Some(tail) => tail.as_ref().next.set(Some(waiter)),
            None => self.head = Some(waiter),
        }
    }

    unsafe fn pop(&mut self) -> Option<NonNull<Waiter>> {
        self.head.map(|head| {
            let waiter = Pin::new_unchecked(head.as_ref());
            assert!(self.try_remove(waiter));
            head
        })
    }

    unsafe fn try_remove(&mut self, waiter: Pin<&Waiter>) -> bool {
        let waiter = NonNull::from(&*waiter);
        if !waiter.as_ref().waiting.replace(false) {
            return false;
        }

        let prev = waiter.as_ref().prev.get();
        let next = waiter.as_ref().next.get();

        if let Some(prev) = prev {
            prev.as_ref().next.set(next);
            match next {
                Some(next) => next.as_ref().prev.set(Some(prev)),
                None => self.tail = Some(prev),
            }
        } else {
            self.head = next;
            if self.head.is_none() {
                self.tail = None;
            }
        }

        true
    }
}

#[derive(Default)]
pub struct Parker {
    pending: AtomicUsize,
    waiters: Mutex<WaitQueue>,
}

impl Parker {
    pub unsafe fn block_on<F: Future>(mut fut: F) -> F::Output {
        Self::block_on_pinned(Pin::new_unchecked(&mut fut))
    }

    unsafe fn block_on_pinned<F: Future>(mut fut: Pin<&mut F>) -> F::Output {
        struct Signal {
            thread: thread::Thread,
            notified: AtomicBool,
            _pin: PhantomPinned,
        }

        let signal = Signal {
            thread: thread::current(),
            notified: AtomicBool::default(),
            _pin: PhantomPinned,
        };
        let signal = Pin::new_unchecked(&signal);

        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr, &VTABLE),
            |ptr| unsafe {
                let signal = &*ptr.cast::<Signal>();
                if !signal.notified.swap(true, Ordering::Release) {
                    signal.thread.unpark();
                }
            },
            |_ptr| unreachable!("wake_by_ref"),
            |_ptr| {},
        );

        let ptr = (&*signal as *const Signal).cast::<()>();
        let waker = Waker::from_raw(RawWaker::new(ptr, &VTABLE));
        let mut cx = Context::from_waker(&waker);

        loop {
            if let Poll::Ready(output) = fut.as_mut().poll(&mut cx) {
                return output;
            }
            while !signal.notified.swap(false, Ordering::Acquire) {
                thread::park();
            }
        }
    }

    pub async fn park(&self, should_park: impl FnOnce() -> bool) {
        struct ParkFuture<'a, 'b> {
            parker: &'a Parker,
            waiter: Option<Pin<&'b Waiter>>,
        }

        impl<'a, 'b> Future for ParkFuture<'a, 'b> {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                let waiter = self
                    .waiter
                    .take()
                    .expect("ParkFuture polled after completion");
                if waiter.waker.poll(cx.waker()).is_ready() {
                    return Poll::Ready(());
                }

                self.waiter = Some(waiter);
                Poll::Pending
            }
        }

        impl<'a, 'b> Drop for ParkFuture<'a, 'b> {
            fn drop(&mut self) {
                if let Some(waiter) = self.waiter.take() {
                    unsafe {
                        if self
                            .parker
                            .waiters
                            .lock()
                            .unwrap()
                            .try_remove(waiter.as_ref())
                        {
                            self.parker.pending.fetch_sub(1, Ordering::Relaxed);
                            return;
                        }

                        self.waiter = Some(waiter);
                        Parker::block_on_pinned(Pin::new(self));
                    }
                }
            }
        }

        self.pending.fetch_add(1, Ordering::SeqCst);
        let mut waiters = self.waiters.lock().unwrap();

        if !should_park() {
            drop(waiters);
            self.pending.fetch_sub(1, Ordering::Relaxed);
            return;
        }

        unsafe {
            let waiter = Waiter::default();
            let waiter = Pin::new_unchecked(&waiter);

            waiters.push(waiter.as_ref());
            drop(waiters);

            ParkFuture {
                parker: self,
                waiter: Some(waiter),
            }
            .await
        }
    }

    pub fn unpark(&self, count: usize) {
        fence(Ordering::SeqCst);
        if self.pending.load(Ordering::Relaxed) == 0 {
            return;
        }

        unsafe {
            let mut popped = 0;
            let mut unparked = None;

            let mut waiters = self.waiters.lock().unwrap();
            for waiter in std::iter::from_fn(|| waiters.pop()).take(count) {
                waiter.as_ref().next.set(unparked);
                unparked = Some(waiter);
                popped += 1;
            }

            drop(waiters);
            if popped > 0 {
                self.pending.fetch_sub(popped, Ordering::Relaxed);
            }

            while let Some(waiter) = unparked {
                unparked = waiter.as_ref().next.get();
                waiter.as_ref().waker.wake();
            }
        }
    }
}
