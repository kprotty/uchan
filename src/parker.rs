use super::event::{Event, TimedEvent};
use core::{
    pin::Pin,
    marker::PhantomPinned,
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicPtr, Ordering},
};

struct EventRef {
    ptr: *const (),
    set_fn: unsafe fn(*const ()),
    _pinned: PhantomPinned,
}

pub(super) struct Parker {
    state: AtomicPtr<EventRef>,
}

impl Parker {
    pub(super) const EMPTY: Self = Self {
        state: AtomicPtr::new(null_mut()),
    };

    pub(super) fn park<E: Event>(&self) {
        unimplemented!("todo")
    }

    pub(super) fn try_park_for<E: TimedEvent>(&self, timeout: E::Duration) -> bool {
        unimplemented!("todo")
    }

    pub(super) fn unpark(&self) {
        unimplemented!("todo")
    }

    pub(super) fn shutdown(&self) {
        unimplemented!("todo")
    }
}
