use core::pin::Pin;

pub unsafe trait Event: Sync {
    fn with(f: impl FnOnce(Pin<&Self>));

    fn wait(self: Pin<&Self>);

    fn set(self: Pin<&Self>);
}

pub trait TimedEvent: Event {
    type Duration;

    fn try_wait_for(self: Pin<&Self>, timeout: Self::Duration) -> bool;
}

#[cfg(feature = "std")]
pub use if_std::*;

#[cfg(feature = "std")]
mod if_std {
    use std::{
        cell::Cell,
        marker::PhantomPinned,
        pin::Pin,
        ptr::{null_mut, NonNull},
        sync::atomic::{AtomicBool, AtomicPtr, Ordering},
        thread,
    };

    pub struct StdEvent {
        thread: Cell<Option<thread::Thread>>,
        unparked: AtomicBool,
        _pinned: PhantomPinned,
    }

    unsafe impl Sync for StdEvent {}

    unsafe impl super::Event for StdEvent {
        fn with(f: impl FnOnce(Pin<&Self>)) {
            let event = Self {
                thread: Cell::new(Some(thread::current())),
                unparked: AtomicBool::new(false),
                _pinned: PhantomPinned,
            };

            // SAFETY: the event is stack allocated and lives only for the closure.
            f(unsafe { Pin::new_unchecked(&event) })
        }

        fn wait(self: Pin<&Self>) {
            while !self.unparked.load(Ordering::Acquire) {
                thread::park();
            }
        }

        fn set(self: Pin<&Self>) {
            let thread = self.thread.take().expect("StdEvent without a thread");
            self.unparked.store(true, Ordering::Release);
            thread.unpark();
        }
    }

    impl TimedEvent for StdEvent {
        type Duration = std::time::Duration;

        fn try_wait_for(self: Pin<&Self>, timeout: Self::Duration) -> bool {
            let mut started = None;
            loop {
                if self.unparked.load(Ordering::Acquire) {
                    return true;
                }

                #[cfg(miri)]
                {
                    std::mem::drop::<Option<()>>(started);
                    return false;
                }

                #[cfg(not(miri))]
                {
                    let now = std::time::Instant::now();
                    let start = started.unwrap_or(now);
                    started = Some(start);

                    match timeout.checked_sub(now.duration_since(start)) {
                        Some(until_timeout) => thread::park_timeout(until_timeout),
                        None => return false,
                    }
                }
            }
        }
    }
}
