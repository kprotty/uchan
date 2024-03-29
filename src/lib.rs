//! Multi-producer, single-consumer FIFO queue communication primitives.
//!
//! This module provides message-based communication over channels, concretely
//! defined among two types:
//!
//! * [`Sender`]
//! * [`Receiver`]
//!
//! A [`Sender`] is used to send data to a [`Receiver`]. Senders are clone-able (multi-producer)
//! such that many threads can send simultaneously to one receiver (single-consumer).
//!
//! There is currently one flavour available: An asynchronous, infinitely buffered channel.
//! The [`channel`] function will return a `(Sender, Receiver)` tuple where all sends will be
//! **asynchronous** (they never block). The channel conceptually has an infinite buffer.
//!
//! ## `no_std` Usage
//!
//! Channels can be used in `no_std` settings due to the thread blocking facilities being made generic.
//! Use the `Event` trait to implement thread parking and create custom [`RawSender`]s and [`Receiver`]s using `raw_channel`.
//! The default [`Sender`] and [`Receiver`] use `StdEvent` which implements thread parking using `std::thread::park`.
//!
//! ## Disconnection
//!
//! The send and receive operations on channels will all return a [`Result`]
//! indicating whether the operation succeeded or not. An unsuccessful operation
//! is normally indicative of the other half of a channel having "hung up" by
//! being dropped in its corresponding thread.
//!
//! Once half of a channel has been deallocated, most operations can no longer
//! continue to make progress, so [`Err`] will be returned. Many applications
//! will continue to [`unwrap`] the results returned from this module,
//! instigating a propagation of failure among threads if one unexpectedly dies.
//!
//! [`unwrap`]: Result::unwrap
//!
//! # Examples
//!
//! Simple usage:
//!
//! ```
//! use std::thread;
//! use uchan::channel;
//!
//! // Create a simple streaming channel
//! let (tx, rx) = channel();
//! thread::spawn(move|| {
//!     tx.send(10).unwrap();
//! });
//! assert_eq!(rx.recv().unwrap(), 10);
//! ```
//!
//! Shared usage:
//!
//! ```
//! use std::thread;
//! use uchan::channel;
//!
//! // Create a shared channel that can be sent along from many threads
//! // where tx is the sending half (tx for transmission), and rx is the receiving
//! // half (rx for receiving).
//! let (tx, rx) = channel();
//! for i in 0..10 {
//!     let tx = tx.clone();
//!     thread::spawn(move|| {
//!         tx.send(i).unwrap();
//!     });
//! }
//!
//! for _ in 0..10 {
//!     let j = rx.recv().unwrap();
//!     assert!(0 <= j && j < 10);
//! }
//! ```
//!
//! Propagating panics:
//!
//! ```
//! use uchan::channel;
//!
//! // The call to recv() will return an error because the channel has already
//! // hung up (or been deallocated)
//! let (tx, rx) = channel::<i32>();
//! drop(tx);
//! assert!(rx.recv().is_err());
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(unstable_name_collisions)]
#![warn(
    rust_2018_idioms,
    unreachable_pub,
    missing_docs,
    missing_debug_implementations
)]

extern crate alloc;

mod backoff;
mod event;
mod parker;
mod queue;

use alloc::sync::Arc;
use core::{fmt, marker::PhantomData};
use queue::Queue;

pub use event::{Event, TimedEvent};

#[cfg(feature = "std")]
pub use if_std::*;

#[cfg(feature = "std")]
mod if_std {
    pub use super::event::StdEvent;

    /// An unbounded channel sender implemented with [`StdEvent`].
    /// See [`RawSender`] for more details.
    ///
    /// [`RawSender`]: super::RawSender
    pub type Sender<T> = super::RawSender<T>;

    /// An unbounded channel receiver implemented with [`StdEvent`].
    /// See [`RawReceiver`] for more details.
    ///
    /// [`RawReceiver`]: super::RawReceiver
    pub type Receiver<T> = super::RawReceiver<StdEvent, T>;

    /// Creates an unbounded channel [`Sender`] and [`Receiver`] using the `StdEvent` implementation.
    /// See [`raw_channel`] for more details.
    ///
    /// [`raw_channel`]: super::raw_channel
    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        super::raw_channel::<StdEvent, T>()
    }
}

/// An error returned from the [`RawSender::send`] function on **channel**s.
///
/// A **send** operation can only fail if the receiving end of a channel is
/// disconnected, implying that the data could never be received. The error
/// contains the data being sent as a payload so it can be recovered.
#[derive(PartialEq, Eq, Clone, Copy)]
pub struct SendError<T>(pub T);

#[cfg(feature = "std")]
impl<T: Send> std::error::Error for SendError<T> {}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendError").finish_non_exhaustive()
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "sending on a closed channel".fmt(f)
    }
}

/// An error returned from the [`recv`] function on a [`RawReceiver`].
///
/// The [`recv`] operation can only fail if the sending half of a
/// [`raw_channel`] is disconnected, implying that no further messages
/// will ever be received.
///
/// [`recv`]: RawReceiver::recv
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct RecvError;

#[cfg(feature = "std")]
impl std::error::Error for RecvError {}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "receiving on a closed channel".fmt(f)
    }
}

/// This enumeration is the list of the possible reasons that [`try_recv`] could
/// not return data when called. This can occur with a [`raw_channel`].
///
/// [`try_recv`]: RawReceiver::try_recv
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum TryRecvError {
    /// This **channel** is currently empty, but the **Sender**(s) have not yet
    /// disconnected, so data may yet become available.
    Empty,

    /// The **channel**'s sending half has become disconnected, and there will
    /// never be any more data received on it.
    Disconnected,
}

#[cfg(feature = "std")]
impl std::error::Error for TryRecvError {}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TryRecvError::Empty => "receiving on an empty channel".fmt(f),
            TryRecvError::Disconnected => "receiving on a closed channel".fmt(f),
        }
    }
}

impl From<RecvError> for TryRecvError {
    /// Converts a `RecvError` into a `TryRecvError`.
    ///
    /// This conversion always returns `TryRecvError::Disconnected`.
    ///
    /// No data is allocated on the heap.
    fn from(err: RecvError) -> TryRecvError {
        match err {
            RecvError => TryRecvError::Disconnected,
        }
    }
}

/// This enumeration is the list of possible errors that made [`recv_timeout`]
/// unable to return data when called. This can occur with a [`raw_channel`].
///
/// [`recv_timeout`]: RawReceiver::recv_timeout
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum RecvTimeoutError {
    /// This **channel** is currently empty, but the **Sender**(s) have not yet
    /// disconnected, so data may yet become available.
    Timeout,
    /// The **channel**'s sending half has become disconnected, and there will
    /// never be any more data received on it.
    Disconnected,
}

#[cfg(feature = "std")]
impl std::error::Error for RecvTimeoutError {}

impl fmt::Display for RecvTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            RecvTimeoutError::Timeout => "timed out waiting on channel".fmt(f),
            RecvTimeoutError::Disconnected => "channel is empty and sending half is closed".fmt(f),
        }
    }
}

impl From<RecvError> for RecvTimeoutError {
    /// Converts a `RecvError` into a `RecvTimeoutError`.
    ///
    /// This conversion always returns `RecvTimeoutError::Disconnected`.
    ///
    /// No data is allocated on the heap.
    fn from(err: RecvError) -> RecvTimeoutError {
        match err {
            RecvError => RecvTimeoutError::Disconnected,
        }
    }
}

/// Creates a new asynchronous channel, returning the sender/receiver halves.
/// All data sent on the [`RawSender`] will become available on the [`RawReceiver`] in
/// the same order as it was sent, and no [`send`] will block the calling thread
/// (this channel has an "infinite buffer"). [`recv`] will block until a message
/// is available while there is at least one [`Sender`] alive (including clones).
///
/// The [`RawSender`] can be cloned to [`send`] to the same channel multiple times, but
/// only one [`RawReceiver`] is supported.
///
/// If the [`RawReceiver`] is disconnected while trying to [`send`] with the
/// [`RawSender`], the [`send`] method will return a [`SendError`]. Similarly, if the
/// [`RawSender`] is disconnected while trying to [`recv`], the [`recv`] method will
/// return a [`RecvError`].
///
/// [`send`]: RawSender::send
/// [`recv`]: RawReceiver::recv
///
/// # Examples
///
/// ```
/// use uchan::channel;
/// use std::thread;
///
/// let (sender, receiver) = channel();
///
/// // Spawn off an expensive computation
/// thread::spawn(move|| {
/// #   fn expensive_computation() {}
///     sender.send(expensive_computation()).unwrap();
/// });
///
/// // Do some useful work for awhile
///
/// // Let's see what that answer was
/// println!("{:?}", receiver.recv().unwrap());
/// ```
pub fn raw_channel<E, T>() -> (RawSender<T>, RawReceiver<E, T>) {
    let queue = Arc::new(Queue::EMPTY);
    let sender = RawSender {
        queue: queue.clone(),
    };
    let receiver = RawReceiver {
        queue,
        _event: PhantomData,
    };
    (sender, receiver)
}

/// The sending-half of Rust's asynchronous [`raw_channel`] type. This half can only be
/// owned by one thread, but it can be cloned to send to other threads.
///
/// Messages can be sent through this channel with [`send`].
///
/// Note: all senders (the original and the clones) need to be dropped for the receiver
/// to stop blocking to receive messages with [`RawReceiver::recv`].
///
/// [`send`]: RawSender::send
///
/// # Examples
///
/// ```rust
/// use uchan::channel;
/// use std::thread;
///
/// let (sender, receiver) = channel();
/// let sender2 = sender.clone();
///
/// // First thread owns sender
/// thread::spawn(move || {
///     sender.send(1).unwrap();
/// });
///
/// // Second thread owns sender2
/// thread::spawn(move || {
///     sender2.send(2).unwrap();
/// });
///
/// let msg = receiver.recv().unwrap();
/// let msg2 = receiver.recv().unwrap();
///
/// assert_eq!(3, msg + msg2);
/// ```
pub struct RawSender<T> {
    queue: Arc<Queue<T>>,
}

impl<T> RawSender<T> {
    /// Attempts to send a value on this channel, returning it back if it could
    /// not be sent.
    ///
    /// A successful send occurs when it is determined that the other end of
    /// the channel has not hung up already. An unsuccessful send would be one
    /// where the corresponding receiver has already been deallocated. Note
    /// that a return value of [`Err`] means that the data will never be
    /// received, but a return value of [`Ok`] does *not* mean that the data
    /// will be received. It is possible for the corresponding receiver to
    /// hang up immediately after this function returns [`Ok`].
    ///
    /// This method will never block the current thread.
    ///
    /// # Examples
    ///
    /// ```
    /// use uchan::channel;
    ///
    /// let (tx, rx) = channel();
    ///
    /// // This send is always successful
    /// tx.send(1).unwrap();
    ///
    /// // This send will fail because the receiver is gone
    /// drop(rx);
    /// assert_eq!(tx.send(1).unwrap_err().0, 1);
    /// ```
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.queue.send(value).map_err(SendError)
    }
}

impl<T> Clone for RawSender<T> {
    /// Clone a sender to send to other threads.
    ///
    /// Note, be aware of the lifetime of the sender because all senders
    /// (including the original) need to be dropped in order for
    /// [`RawReceiver::recv`] to stop blocking.
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
        }
    }
}

impl<T> fmt::Debug for RawSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender").finish_non_exhaustive()
    }
}

impl<T> Drop for RawSender<T> {
    fn drop(&mut self) {
        if Arc::strong_count(&self.queue) == 2 {
            let is_sender = true;
            self.queue.disconnect(is_sender);
        }
    }
}

/// The receiving half of Rust's [`raw_channel`] type.
/// This half can only be owned by one thread.
///
/// Messages sent to the channel can be retrieved using [`recv`].
///
/// [`recv`]: RawReceiver::recv
///
/// # Examples
///
/// ```rust
/// use uchan::channel;
/// use std::thread;
/// use std::time::Duration;
///
/// let (send, recv) = channel();
///
/// thread::spawn(move || {
///     send.send("Hello world!").unwrap();
///     thread::sleep(Duration::from_secs(2)); // block for two seconds
///     send.send("Delayed for 2 seconds").unwrap();
/// });
///
/// println!("{}", recv.recv().unwrap()); // Received immediately
/// println!("Waiting...");
/// println!("{}", recv.recv().unwrap()); // Received after 2 seconds
/// ```
pub struct RawReceiver<E, T> {
    queue: Arc<Queue<T>>,
    _event: PhantomData<E>,
}

impl<E, T> RawReceiver<E, T> {
    /// Attempts to return a pending value on this receiver without blocking.
    ///
    /// This method will never block the caller in order to wait for data to
    /// become available. Instead, this will always return immediately with a
    /// possible option of pending data on the channel.
    ///
    /// This is useful for a flavor of "optimistic check" before deciding to
    /// block on a receiver.
    ///
    /// Compared with [`recv`], this function has two failure cases instead of one
    /// (one for disconnection, one for an empty buffer).
    ///
    /// [`recv`]: Self::recv
    ///
    /// # Examples
    ///
    /// ```rust
    /// use uchan::{Receiver, channel};
    ///
    /// let (_, receiver): (_, Receiver<i32>) = channel();
    ///
    /// assert!(receiver.try_recv().is_err());
    /// ```
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match unsafe { self.queue.try_recv() } {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(TryRecvError::Empty),
            Err(()) => Err(TryRecvError::Disconnected),
        }
    }
}

impl<E: Event, T> RawReceiver<E, T> {
    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up.
    ///
    /// This function will always block the current thread if there is no data
    /// available and it's possible for more data to be sent (at least one sender
    /// still exists). Once a message is sent to the corresponding [`RawSender`],
    /// this receiver will wake up and return that message.
    ///
    /// If the corresponding [`RawSender`] has disconnected, or it disconnects while
    /// this call is blocking, this call will wake up and return [`Err`] to
    /// indicate that no more messages can ever be received on this channel.
    /// However, since channels are buffered, messages sent before the disconnect
    /// will still be properly received.
    ///
    /// # Examples
    ///
    /// ```
    /// use uchan::channel;
    /// use std::thread;
    ///
    /// let (send, recv) = channel();
    /// let handle = thread::spawn(move || {
    ///     send.send(1u8).unwrap();
    /// });
    ///
    /// handle.join().unwrap();
    ///
    /// assert_eq!(Ok(1), recv.recv());
    /// ```
    ///
    /// Buffering behavior:
    ///
    /// ```
    /// use uchan::{channel, RecvError};
    /// use std::thread;
    ///
    /// let (send, recv) = channel();
    /// let handle = thread::spawn(move || {
    ///     send.send(1u8).unwrap();
    ///     send.send(2).unwrap();
    ///     send.send(3).unwrap();
    ///     drop(send);
    /// });
    ///
    /// // wait for the thread to join so we ensure the sender is dropped
    /// handle.join().unwrap();
    ///
    /// assert_eq!(Ok(1), recv.recv());
    /// assert_eq!(Ok(2), recv.recv());
    /// assert_eq!(Ok(3), recv.recv());
    /// assert_eq!(Err(RecvError), recv.recv());
    /// ```
    pub fn recv(&self) -> Result<T, RecvError> {
        (unsafe { self.queue.recv::<E>() }).map_err(|_| RecvError)
    }
}

impl<E: TimedEvent, T> RawReceiver<E, T> {
    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up, or if it waits more than `timeout`.
    ///
    /// This function will always block the current thread if there is no data
    /// available and it's possible for more data to be sent (at least one sender
    /// still exists). Once a message is sent to the corresponding [`RawSender`]
    /// this receiver will wake up and return that message.
    ///
    /// If the corresponding [`RawSender`] has disconnected, or it disconnects while
    /// this call is blocking, this call will wake up and return [`Err`] to
    /// indicate that no more messages can ever be received on this channel.
    /// However, since channels are buffered, messages sent before the disconnect
    /// will still be properly received.
    ///
    /// # Examples
    ///
    /// Successfully receiving value before encountering timeout:
    ///
    /// ```no_run
    /// use std::thread;
    /// use std::time::Duration;
    /// use uchan::channel;
    ///
    /// let (send, recv) = channel();
    ///
    /// thread::spawn(move || {
    ///     send.send('a').unwrap();
    /// });
    ///
    /// assert_eq!(
    ///     recv.recv_timeout(Duration::from_millis(400)),
    ///     Ok('a')
    /// );
    /// ```
    ///
    /// Receiving an error upon reaching timeout:
    ///
    /// ```no_run
    /// use std::thread;
    /// use std::time::Duration;
    /// use uchan::{channel, RecvTimeoutError};
    ///
    /// let (send, recv) = channel();
    ///
    /// thread::spawn(move || {
    ///     thread::sleep(Duration::from_millis(800));
    ///     send.send('a').unwrap();
    /// });
    ///
    /// assert_eq!(
    ///     recv.recv_timeout(Duration::from_millis(400)),
    ///     Err(RecvTimeoutError::Timeout)
    /// );
    /// ```
    pub fn recv_timeout(&self, timeout: E::Duration) -> Result<T, RecvTimeoutError> {
        match unsafe { self.queue.recv_timeout::<E>(timeout) } {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(RecvTimeoutError::Timeout),
            Err(()) => Err(RecvTimeoutError::Disconnected),
        }
    }
}

impl<E, T> fmt::Debug for RawReceiver<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver").finish_non_exhaustive()
    }
}

impl<E, T> Drop for RawReceiver<E, T> {
    fn drop(&mut self) {
        let is_sender = false;
        self.queue.disconnect(is_sender);
    }
}
