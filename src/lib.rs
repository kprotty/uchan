mod atomic;
mod bounded;
mod error;
mod parker;

pub use self::error::{RecvError, SendError, TryRecvError, TrySendError};

#[derive(Debug)]
pub enum Receiver<T> {
    Bounded(bounded::Receiver<T>),
}

impl<T> Receiver<T> {
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self {
            Self::Bounded(ref mut receiver) => receiver.try_recv(),
        }
    }

    pub async fn recv(&mut self) -> Result<T, RecvError> {
        match self {
            Self::Bounded(ref mut receiver) => receiver.recv().await,
        }
    }

    pub fn blocking_recv(&mut self) -> Result<T, RecvError> {
        match self {
            Self::Bounded(ref mut receiver) => receiver.blocking_recv(),
        }
    }
}

pub type SyncSender<T> = bounded::Sender<T>;

pub fn sync_channel<T>(capacity: usize) -> (SyncSender<T>, Receiver<T>) {
    assert_ne!(capacity, 0, "rendezvous channels aren't supported yet");

    let (sender, receiver) = bounded::new(capacity);
    let receiver = Receiver::Bounded(receiver);

    (sender, receiver)
}
