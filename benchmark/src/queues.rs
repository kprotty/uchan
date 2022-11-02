use std::sync::mpsc as std_mpsc;

pub trait Channel: 'static {
    type Sender: ChannelSender;
    type Receiver: ChannelReceiver;

    fn name() -> &'static str;

    fn create() -> (Self::Sender, Self::Receiver);
}

pub trait ChannelSender: Clone + Send + 'static {
    fn send(&self, value: usize);
}

pub trait ChannelReceiver: 'static {
    fn recv(&self) -> usize;
}

macro_rules! impl_channel {
    ($name:expr, $module:ident, $pkg:ident, $constructor:ident) => {
        pub mod $module {
            use super::*;
            pub struct ChannelImpl;

            impl Channel for ChannelImpl {
                type Sender = ChannelSenderImpl;
                type Receiver = ChannelReceiverImpl;

                fn name() -> &'static str {
                    $name
                }

                fn create() -> (ChannelSenderImpl, ChannelReceiverImpl) {
                    let (tx, rx) = $pkg::$constructor::<usize>();
                    (ChannelSenderImpl(tx), ChannelReceiverImpl(rx))
                }
            }

            #[derive(Clone)]
            pub struct ChannelSenderImpl($pkg::Sender<usize>);

            impl ChannelSender for ChannelSenderImpl {
                fn send(&self, value: usize) {
                    self.0.send(value).unwrap()
                }
            }

            pub struct ChannelReceiverImpl($pkg::Receiver<usize>);

            impl ChannelReceiver for ChannelReceiverImpl {
                fn recv(&self) -> usize {
                    self.0.recv().unwrap()
                }
            }
        }
    }
}

impl_channel!("crossbeam", impl_crossbeam, crossbeam_channel, unbounded);
impl_channel!("flume", impl_flume, flume, unbounded);
impl_channel!("uchan", impl_uchan, uchan, channel);
impl_channel!("std", impl_mpsc, std_mpsc, channel);

macro_rules! apply_to_all {
    ($func:ident, $($arg:expr),+ $(,)?) => {
        $func::<crate::queues::impl_uchan::ChannelImpl>($($arg),+);
        $func::<crate::queues::impl_crossbeam::ChannelImpl>($($arg),+);
        $func::<crate::queues::impl_flume::ChannelImpl>($($arg),+);
        $func::<crate::queues::impl_mpsc::ChannelImpl>($($arg),+);
    }
}

pub(crate) use apply_to_all;

