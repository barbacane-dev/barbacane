//! A response body that reports, once, how many data bytes it carried: at its
//! last frame, or when it is dropped before (a client that left).

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use hyper::body::{Body, Frame, SizeHint};

/// `inner`, calling `on_end` with the bytes sent when the body ends.
pub struct MeteredBody<B, F: FnOnce(u64)> {
    inner: B,
    bytes: u64,
    on_end: Option<F>,
}

impl<B, F: FnOnce(u64)> MeteredBody<B, F> {
    pub fn new(inner: B, on_end: F) -> Self {
        Self {
            inner,
            bytes: 0,
            on_end: Some(on_end),
        }
    }

    fn end(&mut self) {
        if let Some(on_end) = self.on_end.take() {
            on_end(self.bytes);
        }
    }
}

impl<B, F> Body for MeteredBody<B, F>
where
    B: Body<Data = Bytes> + Unpin,
    F: FnOnce(u64) + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.bytes += data.len() as u64;
                }
            }
            Poll::Ready(None) | Poll::Ready(Some(Err(_))) => this.end(),
            Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B, F: FnOnce(u64)> Drop for MeteredBody<B, F> {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, StreamBody};
    use std::sync::{Arc, Mutex};

    type Frames = futures_util::stream::Iter<std::vec::IntoIter<Result<Frame<Bytes>, String>>>;

    fn stream(chunks: &[&'static str]) -> StreamBody<Frames> {
        let frames: Vec<Result<Frame<Bytes>, String>> = chunks
            .iter()
            .map(|c| Ok(Frame::data(Bytes::from_static(c.as_bytes()))))
            .collect();
        StreamBody::new(futures_util::stream::iter(frames))
    }

    fn recorder() -> (Arc<Mutex<Vec<u64>>>, impl FnOnce(u64) + Unpin) {
        let ends = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&ends);
        (ends, move |bytes| sink.lock().unwrap().push(bytes))
    }

    #[tokio::test]
    async fn the_bytes_sent_are_reported_once_at_the_end() {
        let (ends, on_end) = recorder();
        let body = MeteredBody::new(stream(&["data: one\n\n", "data: two\n\n"]), on_end);
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(collected.len(), 22);
        assert_eq!(*ends.lock().unwrap(), vec![22]);
    }

    #[tokio::test]
    async fn a_body_dropped_early_reports_what_it_sent() {
        let (ends, on_end) = recorder();
        let mut body = MeteredBody::new(stream(&["abc", "defgh"]), on_end);
        let first = body.frame().await.unwrap().unwrap();
        assert_eq!(first.data_ref().map(Bytes::len), Some(3));
        assert!(ends.lock().unwrap().is_empty(), "not ended yet");
        drop(body);
        assert_eq!(*ends.lock().unwrap(), vec![3]);
    }

    #[test]
    fn a_body_never_polled_reports_zero() {
        let (ends, on_end) = recorder();
        drop(MeteredBody::new(stream(&["unsent"]), on_end));
        assert_eq!(*ends.lock().unwrap(), vec![0]);
    }
}
