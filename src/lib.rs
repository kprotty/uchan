#![allow(unstable_name_collisions)]
#![warn(
    rust_2018_idioms,
    unreachable_pub
)]

use std::{fmt, Arc, num::NonZeroUsize, cell::Cell, time::Duration};

mod unbounded;
mod bounded;
mod rendezvous;

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(unbounded::Channel::new());
    let sender = Sender {
        chan: chan.clone(),
        _not_sync: Cell::new(()),
    };
    
    let receiver = Receiver { inner: Chan::Unbounded(chan) };
    (sender, receiver)
}

pub fn sync_channel<T>(capacity: usize) -> (SyncSender<T>, Receiver<T>) {
    match NonZeroUsize::new(capacity) {
        None => {
            let chan = Arc::new(rendezvous::Channel::new());
            let sender = SyncSender { inner: BoundedChan::Rendezvous(chan.clone()) };
            let receiver = Receiver { inner: Chan::Rendezvous(chan) };
            (sender, receiver)
        },
        Some(capacity) => {
            let chan = Arc::new(bounded::Channel::new(capacity));
            let sender = SyncSender { inner: BoundedChan::Bounded(chan.clone()) };
            let receiver = Receiver { inner: Chan::Bounded(chan) };
            (sender, receiver)
        }
    }
}

/// An error returned from the [`Sender::send`] or [`SyncSender::send`]
/// function on **channel**s.
///
/// A **send** operation can only fail if the receiving end of a channel is
/// disconnected, implying that the data could never be received. The error
/// contains the data being sent as a payload so it can be recovered.
#[derive(PartialEq, Eq, Clone, Copy)]
pub struct SendError<T>(pub T);

pub struct Sender<T> {
    chan: Arc<unbounded::Channel<T>>,
    _not_sync: Cell<()>,
}

impl<T> Sender<T> {
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        match self.chan.send(value) {
            Ok(()) => Ok(()),
            Err(value) => Err(SendError(value)),
        }
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender").finish_non_exhaustive()
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            chan: Arc::clone(&self.chan),
            _not_sync: Cell::new(()),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 2 {
            self.chan.disconnect(true);
        }
    }
}

enum BoundedChan<T> {
    Bounded(Arc<bounded::Channel<T>>),
    Rendezvous(Arc<rendezvous::Channel<T>>),
}

/// The sending-half of Rust's synchronous [`sync_channel`] type.
///
/// Messages can be sent through this channel with [`send`] or [`try_send`].
///
/// [`send`] will block if there is no space in the internal buffer.
///
/// [`send`]: SyncSender::send
/// [`try_send`]: SyncSender::try_send
///
/// # Examples
///
/// ```rust
/// use std::sync::mpsc::sync_channel;
/// use std::thread;
///
/// // Create a sync_channel with buffer size 2
/// let (sync_sender, receiver) = sync_channel(2);
/// let sync_sender2 = sync_sender.clone();
///
/// // First thread owns sync_sender
/// thread::spawn(move || {
///     sync_sender.send(1).unwrap();
///     sync_sender.send(2).unwrap();
/// });
///
/// // Second thread owns sync_sender2
/// thread::spawn(move || {
///     sync_sender2.send(3).unwrap();
///     // thread will now block since the buffer is full
///     println!("Thread unblocked!");
/// });
///
/// let mut msg;
///
/// msg = receiver.recv().unwrap();
/// println!("message {msg} received");
///
/// // "Thread unblocked!" will be printed now
///
/// msg = receiver.recv().unwrap();
/// println!("message {msg} received");
///
/// msg = receiver.recv().unwrap();
///
/// println!("message {msg} received");
/// ```
pub struct SyncSender<T> {
    inner: BoundedChan<T>,
}

impl<T> fmt::Debug for SyncSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncSender").finish_non_exhaustive()
    }
}

impl<T> SyncSender<T> {
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        let result = match &self.inner {
            BoundedChan::Bounded(chan) => chan.try_send(value),
            BoundedChan::Rendezvous(chan) => chan.try_send(value),
        };

        match result {
            Ok(()) => Ok(()),
            Err(Ok(value)) => Err(TrySendError::Full(value)),
            Err(Err(value)) => Err(TrySendError::Disconnected(value)),
        }
    }

    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let result = match &self.inner {
            BoundedChan::Bounded(chan) => chan.send(value),
            BoundedChan::Rendezvous(chan) => chan.send(value),
        };

        match result {
            Ok(()) => Ok(()),
            Err(value) => Err(SendError(value)),
        }
    }
}

impl<T> Clone for SyncSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: match &self.inner {
                BoundedChan::Bounded(chan) => BoundedChan::Bounded(chan.clone()),
                BoundedChan::Rendezvous(chan) => BoundedChan::Rendezvous(chan.clone()),
            }
        }
    }
}

impl<T> Drop for SyncSender<T> {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 2 {
            match &self.inner {
                BoundedChan::Bounded(chan) => chan.disconnect(true),
                BoundedChan::Rendezvous(chan) => chan.disconnect(true),
            }
        }
    }
}

enum Chan<T> {
    Unbounded(Arc<unbounded::Channel<T>>),
    Bounded(Arc<bounded::Channel<T>>),
    Rendezvous(Arc<rendezvous::Channel<T>>),
}

pub struct Receiver<T> {
    inner: Chan<T>,
}

impl<T> Receiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let result = unsafe {
            match &self.inner {
                Chan::Unbounded(chan) => chan.try_recv(),
                Chan::Bounded(chan) => chan.try_recv(),
                Chan::Rendezvous(chan) => chan.try_recv(),
            }
        };

        match result {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(TryRecvError::Empty),
            Err(()) => Err(TryRecvError::Disconnected),
        }
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        let result = unsafe {
            match &self.inner {
                Chan::Unbounded(chan) => chan.recv(),
                Chan::Bounded(chan) => chan.recv(),
                Chan::Rendezvous(chan) => chan.recv(),
            }
        };

        match result {
            Ok(value) => Ok(value),
            Err(()) => Err(RecvError),
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let result = unsafe {
            match &self.inner {
                Chan::Unbounded(chan) => chan.recv_timeout(timeout),
                Chan::Bounded(chan) => chan.recv_timeout(timeout),
                Chan::Rendezvous(chan) => chan.recv_timeout(timeout),
            }
        };

        match result {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(RecvTimeoutError::Timeout),
            Err(()) => Err(RecvTimeoutError::Disconnected),
        }
    }

    pub fn iter(&self) -> Iter<'_, T> {
        Iter { receiver: self }
    }

    pub fn try_iter(&self) -> TryIter<'_, T> {
        TryIter { receiver: self }
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver").finish_non_exhaustive()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        match &self.inner {
            Chan::Unbounded(chan) => chan.disconnect(false),
            Chan::Bounded(chan) => chan.disconnect(false),
            Chan::Rendezvous(chan) => chan.disconnect(false),
        }
    }
}

struct TryIter<'a, T> {
    receiver: &'a Receiver<T>,
}

impl<'a, T> Iterator for TryIter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.receiver.try_recv().ok()
    }
}

struct Iter<'a, T> {
    receiver: &'a Receiver<T>,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.receiver.recv().ok()
    }
}

impl<'a, T> IntoIterator for &'a Receiver<T> {
    type Item = T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

struct IntoIter<T> {
    receiver: Receiver<T>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.receiver.recv().ok()
    }
}

impl<T> IntoIterator for Receiver<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> IntoIter<T> {
        IntoIter { receiver: self }
    }
}

