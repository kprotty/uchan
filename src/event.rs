use core::pin::Pin;

/// An Event represents a single-producer, single-consumer notification synchronization primitive.
/// A thread creates an Event using [`Event::with`], makes it available to the producer, and uses
/// [`Event::wait`] to block the caller thread until [`Event::set`] is called from another thread
/// or from the same consumer thread prior to [`Event::wait`].
///
/// The same thread which call [`Event::with`] will always be the same thread that waits.
/// [`Event::set`] will also only be called by a single other "producer" thread, allowing for
/// implementations to rely on any SPSC-style optimizations. Events are Pinned in case the
/// implementation requires a stable address or utilizes intrusive memory.
///
/// ## Safety
///
/// [`Event::with`] **must** block until [`Event::set`] is called. Failure to uphold this invariant
/// could result in undefined behavior at safe use sites.
pub unsafe trait Event: Sync {
    /// Construct a pinned Event, allowing the closure to call [`wait`] on the Event reference.
    ///
    /// [`wait`]: Self::wait
    fn with(f: impl FnOnce(Pin<&Self>));

    /// Blocks the caller until the another thread (or previously on the same thread)
    /// invokes [`Event::set`].
    fn wait(self: Pin<&Self>);

    /// Marks the thread as active and unblocks the waiting thread if any.
    /// Further attempts to call [`Event::wait`] should return immediately.
    fn set(self: Pin<&Self>);
}

/// A TimedEvent is an Event which supports waiting for [`Event::set`] but with a timeout.
/// Most conditions of [`Event::wait`] still apply, but the timeout allows the caller to
/// stop blocking before [`Event::set`] is called from another producer thread.
pub trait TimedEvent: Event {
    /// A timeout value used to represent the maximum amount of time to block the caller when waiting.
    type Duration;

    /// Similar to [`Event::wait`], but takes in a maximum amount of time to block in case the Event is not set.
    /// Returns true if the Event was set and returns false if the caller timed out trying to wait for the Event to be set.
    fn try_wait_for(self: Pin<&Self>, timeout: Self::Duration) -> bool;
}

#[cfg(feature = "std")]
pub use if_std::*;

#[cfg(feature = "std")]
mod if_std {
    use std::{
        cell::Cell,
        fmt,
        marker::PhantomPinned,
        pin::Pin,
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };

    /// An implementation of [`Event`] and [`TimedEvent`] using `std::thread`.
    ///
    /// [`Event`]: super::Event
    /// [`TimedEvent`]: super::TimedEvent
    pub struct StdEvent {
        thread: Cell<Option<thread::Thread>>,
        unparked: AtomicBool,
        _pinned: PhantomPinned,
    }

    impl fmt::Debug for StdEvent {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("StdEvent").finish()
        }
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

    impl super::TimedEvent for StdEvent {
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
