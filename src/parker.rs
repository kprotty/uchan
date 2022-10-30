use super::event::{Event, TimedEvent};
use core::{
    marker::PhantomPinned,
    pin::Pin,
    ptr::{null_mut, read, NonNull},
    sync::atomic::{AtomicPtr, Ordering},
};

struct EventRef {
    _pinned: PhantomPinned,
    ptr: NonNull<()>,
    set_fn: unsafe fn(NonNull<()>),
}

pub(super) struct Parker {
    state: AtomicPtr<EventRef>,
}

impl Parker {
    pub(super) const EMPTY: Self = Self {
        state: AtomicPtr::new(null_mut()),
    };

    pub(super) fn park<E: Event>(&self) {
        assert!(self.park_with::<E>(|event| {
            event.wait();
            true
        }))
    }

    pub(super) fn try_park_for<E: TimedEvent>(
        &self,
        timeout: E::Duration,
    ) -> Result<Option<E::Duration>, ()> {
        let mut timeout = Some(timeout);
        let unparked = self.park_with::<E>(|event| {
            let duration = timeout.take().unwrap();
            event.try_wait_for(duration)
        });

        if !unparked {
            assert!(timeout.is_none());
            return Err(());
        }

        Ok(timeout.take())
    }

    const UNPARKED: *mut EventRef = NonNull::dangling().as_ptr();

    fn park_with<E: Event>(&self, do_park: impl FnOnce(Pin<&E>) -> bool) -> bool {
        let mut unparked = true;
        let mut state = self.state.load(Ordering::Acquire);

        if state.is_null() {
            E::with(|event| {
                let event_ref = EventRef {
                    _pinned: PhantomPinned,
                    ptr: NonNull::from(&*event).cast(),
                    set_fn: |ptr| unsafe {
                        let ptr = ptr.cast::<E>();
                        Pin::new_unchecked(ptr.as_ref()).set();
                    },
                };

                let pinned = unsafe { Pin::new_unchecked(&event_ref) };
                let event_ref_ptr = NonNull::from(&*pinned).as_ptr();
                assert_ne!(event_ref_ptr, Self::UNPARKED);

                if let Err(e) = self.state.compare_exchange(
                    null_mut(),
                    event_ref_ptr,
                    Ordering::Release,
                    Ordering::Acquire,
                ) {
                    state = e;
                    return;
                }

                if do_park(event.as_ref()) {
                    state = self.state.load(Ordering::Acquire);
                    return;
                }

                match self.state.compare_exchange(
                    event_ref_ptr,
                    null_mut(),
                    Ordering::Acquire,
                    Ordering::Acquire,
                ) {
                    Ok(_) => unparked = false,
                    Err(e) => {
                        state = e;
                        event.wait();
                    }
                }
            });
        }

        if unparked {
            assert_eq!(state, Self::UNPARKED);
            self.state.store(null_mut(), Ordering::Relaxed);
        }

        unparked
    }

    pub(super) fn unpark(&self) {
        let mut state = self.state.load(Ordering::SeqCst);

        if !state.is_null() && (state != Self::UNPARKED) {
            state = self.state.swap(Self::UNPARKED, Ordering::AcqRel);
        }

        if !state.is_null() && (state != Self::UNPARKED) {
            unsafe {
                let event_ref = read(state);
                (event_ref.set_fn)(event_ref.ptr);
            }
        }
    }
}
