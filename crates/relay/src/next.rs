//! The one stream combinator this crate needs.
//!
//! [`Stream`] lives in `futures-core`, which has no dependencies of its own.
//! `futures-util` adds the `.next()` helper, but it also pulls in a
//! proc-macro chain (`futures-macro`, and through it `syn`, `quote`, and
//! `proc-macro2`) that every consumer then compiles. That is a poor trade
//! for one combinator, so the combinator lives here instead.
//!
//! [`Stream`]: futures_core::Stream

/// Resolves to the next item of a stream, or `None` when it ends.
///
/// This is the whole of `StreamExt::next` for an [`Unpin`] stream: the
/// bound lets the future hold `&mut St` and pin it on each poll, so no
/// projection machinery is needed.
pub struct Next<'a, St>(pub &'a mut St);

impl<St> std::future::Future for Next<'_, St>
where
    St: futures_core::Stream + Unpin,
{
    type Output = Option<St::Item>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut *self.0).poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that yields the numbers it was built with, then ends.
    struct Counter(Vec<u8>);

    impl futures_core::Stream for Counter {
        type Item = u8;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<u8>> {
            std::task::Poll::Ready(if self.0.is_empty() {
                None
            } else {
                Some(self.0.remove(0))
            })
        }
    }

    /// A stream that returns `Pending` once before each item.
    struct Stalling {
        items: Vec<u8>,
        ready: bool,
    }

    impl futures_core::Stream for Stalling {
        type Item = u8;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<u8>> {
            if self.ready {
                self.ready = false;
                return std::task::Poll::Ready(if self.items.is_empty() {
                    None
                } else {
                    Some(self.items.remove(0))
                });
            }
            self.ready = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }

    /// Drives a future to completion without an executor.
    struct Spin;

    impl Spin {
        fn run<F: std::future::Future>(future: F) -> F::Output {
            let mut future = Box::pin(future);
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            loop {
                if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                    return value;
                }
            }
        }
    }

    #[test]
    fn a_stream_yields_its_items_in_order() {
        let mut stream = Counter(vec![1, 2, 3]);
        assert_eq!(Spin::run(Next(&mut stream)), Some(1));
        assert_eq!(Spin::run(Next(&mut stream)), Some(2));
        assert_eq!(Spin::run(Next(&mut stream)), Some(3));
    }

    #[test]
    fn an_exhausted_stream_yields_none() {
        let mut stream = Counter(Vec::new());
        assert_eq!(Spin::run(Next(&mut stream)), None);
    }

    // A combinator that mishandled `Pending` would hang or lose an item, so
    // this drives a stream that stalls before every single yield.
    #[test]
    fn a_pending_stream_is_polled_again_rather_than_dropped() {
        let mut stream = Stalling {
            items: vec![7, 8],
            ready: false,
        };
        assert_eq!(Spin::run(Next(&mut stream)), Some(7));
        assert_eq!(Spin::run(Next(&mut stream)), Some(8));
        assert_eq!(Spin::run(Next(&mut stream)), None);
    }
}
