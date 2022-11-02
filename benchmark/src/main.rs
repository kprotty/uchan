#![feature(concat_idents)]

mod queues;

pub fn main() {
    queues::apply_to_all!(example, 0);
}

fn example<T: queues::Channel>(x: usize) {
    println!("{:?} {:?}", std::any::TypeId::of::<T>(), x);
}