//! Where background threads send what they produce.

use std::sync::Arc;
use std::sync::mpsc::Sender;

/// Takes a thread's results one by one. Returns false once nobody listens
/// any more (the thread then stops).
pub type Sink<E> = Arc<dyn Fn(E) -> bool + Send + Sync>;

/// A sink feeding a channel, converting events on the way (`|e| e` keeps
/// them as they are).
pub fn channel_sink<T, E: Send + 'static>(tx: Sender<E>, wrap: impl Fn(T) -> E + Send + Sync + 'static) -> Sink<T> {
    Arc::new(move |e| tx.send(wrap(e)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feeds_the_channel_until_it_closes() {
        let (tx, rx) = std::sync::mpsc::channel();
        let sink = channel_sink(tx, |n: u32| n * 2);
        assert!(sink(3));
        assert_eq!(rx.recv().unwrap(), 6);
        drop(rx);
        assert!(!sink(4));
    }
}
