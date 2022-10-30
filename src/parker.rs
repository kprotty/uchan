use super::event::{Event, TimedEvent};
use core::{
    marker::PhantomPinned,
    pin::Pin,
    ptr::{null_mut, read, NonNull},
    sync::atomic::{AtomicPtr, Ordering},
};
use sptr::invalid_mut;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(super) enum ParkResult {
    Unparked,
    Shutdown,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(super) enum TryParkResult {
    Unparked,
    Timeout,
    Shutdown,
}

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

    pub(super) fn park<E: Event>(&self) -> ParkResult {
        match self.park_with::<E>(|event| {
            event.wait();
            true
        }) {
            Ok(true) => ParkResult::Unparked,
            Ok(false) => unreachable!(),
            Err(()) => ParkResult::Shutdown,
        }
    }

    pub(super) fn try_park_for<E: TimedEvent>(&self, timeout: E::Duration) -> TryParkResult {
        match self.park_with::<E>(|event| event.try_wait_for(timeout)) {
            Ok(true) => TryParkResult::Unparked,
            Ok(false) => TryParkResult::Timeout,
            Err(()) => TryParkResult::Shutdown,
        }
    }

    const UNPARKED: *mut EventRef = invalid_mut(0x1);
    const SHUTDOWN: *mut EventRef = invalid_mut(0x2);

    fn park_with<E: Event>(&self, do_park: impl FnOnce(Pin<&E>) -> bool) -> Result<bool, ()> {
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
                assert_ne!(event_ref_ptr, Self::SHUTDOWN);

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

        if state.is_null() {
            assert!(!unparked);
            return Ok(false);
        }

        if state == Self::UNPARKED {
            state = self
                .state
                .compare_exchange(
                    Self::UNPARKED,
                    null_mut(),
                    Ordering::Acquire,
                    Ordering::Acquire,
                )
                .unwrap_or_else(|e| e);
        }

        match state {
            Self::UNPARKED => Ok(true),
            Self::SHUTDOWN => Err(()),
            _ => unreachable!(),
        }
    }

    pub(super) fn unpark(&self) {
        let mut state = self.state.load(Ordering::SeqCst);

        while (state != Self::UNPARKED) && (state != Self::SHUTDOWN) {
            match self.state.compare_exchange_weak(
                state,
                Self::UNPARKED,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => {
                    return unsafe {
                        let event_ref = read(state);
                        (event_ref.set_fn)(event_ref.ptr)
                    }
                }
            }
        }
    }

    pub(super) fn is_shutdown(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::SHUTDOWN
    }

    pub(super) fn shutdown(&self) {
        let state = self.state.swap(Self::SHUTDOWN, Ordering::AcqRel);
        if (state == Self::UNPARKED) || (state == Self::SHUTDOWN) {
            return;
        }

        unsafe {
            let event_ref = read(state);
            (event_ref.set_fn)(event_ref.ptr)
        }
    }
}
