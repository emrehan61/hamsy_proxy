//! Body wrappers: streaming capture (tee), full-buffer capture, and
//! bandwidth throttling.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use tokio::time::{Instant, Sleep};

use crate::error::{ProxyError, Result};

/// Shared, continuously-updated capture state written to by a [`TeeBody`]
/// as frames pass through it.
///
/// Because this is a plain shared slot (rather than a one-shot callback),
/// there is nothing to "leak" if the body is dropped mid-stream (e.g. the
/// client disconnects): whatever was captured so far remains readable via
/// [`TeeState::snapshot`] at any time.
#[derive(Debug, Default)]
pub struct TeeState {
    captured: BytesMut,
    total: u64,
    truncated: bool,
}

impl TeeState {
    /// Returns `(captured_bytes, total_bytes_seen, truncated)`. `captured`
    /// may be shorter than `total` if the cap was reached.
    pub fn snapshot(&self) -> (Bytes, u64, bool) {
        (
            Bytes::copy_from_slice(&self.captured),
            self.total,
            self.truncated,
        )
    }
}

/// A body wrapper that streams every frame through to the caller unmodified
/// while simultaneously recording a capped copy for flow capture.
///
/// Once the captured copy reaches `cap` bytes, further bytes stop being
/// retained (`truncated` is set), but `total` keeps counting so accounting
/// (e.g. for `BodyPayload::size`) stays accurate even for arbitrarily long
/// streaming bodies (SSE, chunked transfer, long-poll, ...).
pub struct TeeBody<B> {
    inner: B,
    cap: usize,
    state: Arc<Mutex<TeeState>>,
}

impl<B> TeeBody<B> {
    /// Wraps `inner`, returning the wrapped body plus a shared handle the
    /// caller can read at any time (mid-stream, after completion, or after
    /// an abort) to see what's been captured so far.
    pub fn new(inner: B, cap: usize) -> (Self, Arc<Mutex<TeeState>>) {
        let state = Arc::new(Mutex::new(TeeState::default()));
        (
            TeeBody {
                inner,
                cap,
                state: state.clone(),
            },
            state,
        )
    }
}

impl<B> Body for TeeBody<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &poll {
            if let Some(data) = frame.data_ref() {
                let mut state = this.state.lock();
                state.total += data.len() as u64;
                if !state.truncated {
                    let remaining = this.cap.saturating_sub(state.captured.len());
                    if remaining == 0 {
                        state.truncated = true;
                    } else if data.len() <= remaining {
                        state.captured.extend_from_slice(data);
                    } else {
                        state.captured.extend_from_slice(&data[..remaining]);
                        state.truncated = true;
                    }
                }
            }
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Fully buffers `body` up to `hard_cap` bytes and returns
/// `(bytes, total_len, truncated)`. Used when a rule needs the whole body
/// materialized before mutation (see the crate-level buffering-decision
/// doc in `http.rs`); unlike [`TeeBody`] this does not stream anything
/// through concurrently, since the caller can't forward a body it hasn't
/// decided how to mutate yet.
pub async fn collect_capped<B>(mut body: B, hard_cap: usize) -> Result<(Bytes, u64, bool)>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    let mut buf = BytesMut::new();
    let mut total: u64 = 0;
    let mut truncated = false;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| ProxyError::Other(format!("body read error: {e}")))?;
        let Some(data) = frame.data_ref() else {
            continue;
        };
        total += data.len() as u64;
        if truncated {
            continue;
        }
        let remaining = hard_cap.saturating_sub(buf.len());
        if remaining == 0 {
            truncated = true;
        } else if data.len() <= remaining {
            buf.extend_from_slice(data);
        } else {
            buf.extend_from_slice(&data[..remaining]);
            truncated = true;
        }
    }
    Ok((buf.freeze(), total, truncated))
}

/// A simple token-bucket rate limiter, decoupled from any specific clock so
/// its refill/withdrawal math can be unit-tested deterministically.
///
/// The bucket starts full (one second's worth of burst allowance) and
/// refills continuously at `rate` bytes/sec, capped at that same one-second
/// burst.
struct TokenBucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
}

impl TokenBucket {
    fn new(bytes_per_sec: u64) -> Self {
        let rate = (bytes_per_sec.max(1)) as f64;
        TokenBucket {
            rate,
            capacity: rate,
            tokens: rate,
        }
    }

    /// Advances the bucket's clock by `elapsed_secs`, refilling tokens.
    fn refill(&mut self, elapsed_secs: f64) {
        self.tokens = (self.tokens + elapsed_secs * self.rate).min(self.capacity);
    }

    /// Attempts to withdraw `amount` tokens.
    ///
    /// Returns `None` (and withdraws immediately) if enough tokens are
    /// already available. Otherwise drains the bucket to zero and returns
    /// `Some(wait_secs)`: how long the caller must wait before the
    /// withdrawal would have succeeded.
    fn try_take(&mut self, amount: f64) -> Option<f64> {
        if self.tokens >= amount {
            self.tokens -= amount;
            None
        } else {
            let deficit = amount - self.tokens;
            self.tokens = 0.0;
            Some(deficit / self.rate)
        }
    }
}

/// A body wrapper that caps transfer speed to a configured `bytes_per_sec`,
/// used for `Action::Throttle`/`throttle_bps` outcomes.
///
/// Throttling is applied per-frame (sleep before releasing a frame once the
/// token bucket can't cover its size) rather than by splitting frames into
/// smaller pieces, which is simple and accurate on average but means actual
/// burstiness follows the wrapped body's natural frame sizes.
pub struct Throttled<B> {
    inner: B,
    bucket: TokenBucket,
    last_refill: Instant,
    /// A frame we've already pulled from `inner` but are holding until its
    /// sleep elapses, plus that sleep future.
    pending: Option<(Pin<Box<Sleep>>, Frame<Bytes>)>,
}

impl<B> Throttled<B> {
    /// Wraps `inner`, capping its data throughput at `bytes_per_sec`.
    pub fn new(inner: B, bytes_per_sec: u64) -> Self {
        Throttled {
            inner,
            bucket: TokenBucket::new(bytes_per_sec),
            last_refill: Instant::now(),
            pending: None,
        }
    }
}

impl<B> Body for Throttled<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();

        // Finish waiting out any sleep left over from a previous poll before
        // pulling anything new from the inner body.
        if let Some((mut sleep, frame)) = this.pending.take() {
            return match sleep.as_mut().poll(cx) {
                Poll::Pending => {
                    this.pending = Some((sleep, frame));
                    Poll::Pending
                }
                Poll::Ready(()) => Poll::Ready(Some(Ok(frame))),
            };
        }

        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                let Some(data) = frame.data_ref() else {
                    // Trailers/non-data frames pass straight through.
                    return Poll::Ready(Some(Ok(frame)));
                };
                let now = Instant::now();
                this.bucket
                    .refill(now.duration_since(this.last_refill).as_secs_f64());
                this.last_refill = now;

                match this.bucket.try_take(data.len() as f64) {
                    None => Poll::Ready(Some(Ok(frame))),
                    Some(wait_secs) => {
                        let mut sleep =
                            Box::pin(tokio::time::sleep(Duration::from_secs_f64(wait_secs)));
                        match sleep.as_mut().poll(cx) {
                            Poll::Pending => {
                                this.pending = Some((sleep, frame));
                                Poll::Pending
                            }
                            Poll::Ready(()) => Poll::Ready(Some(Ok(frame))),
                        }
                    }
                }
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.pending.is_none() && self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// A body wrapper that runs a callback exactly once, when the wrapped body
/// is either fully, cleanly drained (`poll_frame` yields `None`) or dropped
/// before that happens (e.g. a client disconnects mid-response).
///
/// This is how flow finalization is wired up for streamed (non-buffered)
/// bodies: the callback reads whatever a paired [`TeeBody`]/[`TeeState`]
/// captured so far and writes it back into the flow store, so a flow is
/// always finalized with whatever was captured - never left dangling -
/// regardless of whether the stream ended cleanly or was aborted.
pub struct FinalizeBody<B, F: FnOnce() + Send + Unpin + 'static> {
    inner: B,
    on_done: Option<F>,
}

impl<B, F: FnOnce() + Send + Unpin + 'static> FinalizeBody<B, F> {
    /// Wraps `inner`, arranging for `on_done` to run exactly once, on
    /// completion or drop, whichever comes first.
    pub fn new(inner: B, on_done: F) -> Self {
        FinalizeBody {
            inner,
            on_done: Some(on_done),
        }
    }
}

impl<B, F> Body for FinalizeBody<B, F>
where
    B: Body<Data = Bytes> + Unpin,
    F: FnOnce() + Send + Unpin + 'static,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_frame(cx);
        if let Poll::Ready(None) = &poll {
            if let Some(f) = this.on_done.take() {
                f();
            }
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B, F: FnOnce() + Send + Unpin + 'static> Drop for FinalizeBody<B, F> {
    fn drop(&mut self) {
        if let Some(f) = self.on_done.take() {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn token_bucket_allows_burst_up_to_capacity() {
        let mut bucket = TokenBucket::new(1000);
        // Starts full: a withdrawal at or under capacity succeeds immediately.
        assert_eq!(bucket.try_take(1000.0), None);
        // Now empty: the next withdrawal must wait.
        let wait = bucket.try_take(500.0).expect("should need to wait");
        assert!((wait - 0.5).abs() < 1e-9);
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(100);
        bucket.try_take(100.0);
        bucket.refill(0.5); // half a second at 100 B/s = 50 tokens
        assert_eq!(bucket.try_take(50.0), None);
        assert!(bucket.try_take(1.0).is_some());
    }

    #[test]
    fn token_bucket_refill_caps_at_capacity() {
        let mut bucket = TokenBucket::new(100);
        bucket.refill(10.0); // way more than one second's worth
        assert_eq!(bucket.try_take(100.0), None);
        // Bucket should now be empty, not still holding leftover tokens.
        assert!(bucket.try_take(1.0).is_some());
    }

    #[tokio::test]
    async fn tee_body_streams_through_and_captures() {
        let body = Full::new(Bytes::from_static(b"hello world"));
        let (tee, state) = TeeBody::new(body, 1024);
        let (bytes, total, truncated) = collect_capped(tee, 1024).await.unwrap();
        assert_eq!(bytes.as_ref(), b"hello world");
        assert_eq!(total, 11);
        assert!(!truncated);
        let (captured, snap_total, snap_truncated) = state.lock().snapshot();
        assert_eq!(captured.as_ref(), b"hello world");
        assert_eq!(snap_total, 11);
        assert!(!snap_truncated);
    }

    #[tokio::test]
    async fn tee_body_stops_retaining_after_cap_but_keeps_counting() {
        let body = Full::new(Bytes::from_static(b"0123456789"));
        let (tee, state) = TeeBody::new(body, 4);
        let (bytes, total, truncated) = collect_capped(tee, 100).await.unwrap();
        // Full stream is still delivered to the caller uncapped.
        assert_eq!(bytes.as_ref(), b"0123456789");
        assert_eq!(total, 10);
        assert!(!truncated); // collect_capped's own cap (100) wasn't hit.
        let (captured, snap_total, snap_truncated) = state.lock().snapshot();
        assert_eq!(captured.as_ref(), b"0123");
        assert_eq!(snap_total, 10);
        assert!(snap_truncated);
    }

    #[tokio::test]
    async fn collect_capped_truncates_at_hard_cap() {
        let body = Full::new(Bytes::from_static(b"0123456789"));
        let (bytes, total, truncated) = collect_capped(body, 5).await.unwrap();
        assert_eq!(bytes.as_ref(), b"01234");
        assert_eq!(total, 10);
        assert!(truncated);
    }

    #[tokio::test]
    async fn throttled_delivers_all_bytes_unmodified() {
        let body = Full::new(Bytes::from_static(b"throttle me please"));
        let throttled = Throttled::new(body, 1_000_000); // fast enough to not actually block the test
        let (bytes, total, truncated) = collect_capped(throttled, 1024).await.unwrap();
        assert_eq!(bytes.as_ref(), b"throttle me please");
        assert_eq!(total, 18);
        assert!(!truncated);
    }

    #[tokio::test]
    async fn finalize_body_runs_callback_on_clean_completion() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = ran.clone();
        let body = Full::new(Bytes::from_static(b"done"));
        let finalize = FinalizeBody::new(body, move || ran2.store(true, Ordering::SeqCst));
        let (bytes, _, _) = collect_capped(finalize, 1024).await.unwrap();
        assert_eq!(bytes.as_ref(), b"done");
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn finalize_body_runs_callback_on_drop_without_completion() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = ran.clone();
        let body = Full::new(Bytes::from_static(b"never read"));
        let finalize = FinalizeBody::new(body, move || ran2.store(true, Ordering::SeqCst));
        drop(finalize);
        assert!(ran.load(Ordering::SeqCst));
    }
}
