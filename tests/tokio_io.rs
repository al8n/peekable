//! Integration tests for the tokio `AsyncPeekable`.
//!
//! Includes regression tests for bugs found in the deep review:
//! - Bug 1: `poll_peek` no longer double-puts buffered data on Pending.
//! - Bug 2: `poll_read` no longer leaks buffered data on Pending nor
//!   leaves it for re-emission on a later call.
//! - Bug 3: error path no longer mis-indexes into `buf.inner_mut()`.

#![cfg(feature = "tokio")]
#![allow(warnings)]

use std::io::{self, Cursor, ErrorKind};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use ::tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use bytes::BytesMut;
use peekable::tokio::{AsyncPeek, AsyncPeekExt, AsyncPeekable};

// ---- Flaky reader -------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Action {
  Pending, // return Pending without registering a wake (test will explicit-wake)
  Interrupted,
  OtherErr,
  ReadFromInner, // read normally
}

struct FlakyReader {
  inner: Cursor<Vec<u8>>,
  plan: Vec<Action>,
  call: Arc<AtomicUsize>,
}

impl FlakyReader {
  fn new(data: Vec<u8>, plan: Vec<Action>) -> Self {
    Self {
      inner: Cursor::new(data),
      plan,
      call: Arc::new(AtomicUsize::new(0)),
    }
  }
  fn calls(&self) -> Arc<AtomicUsize> {
    self.call.clone()
  }
}

impl AsyncRead for FlakyReader {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    let i = self.call.fetch_add(1, Ordering::SeqCst);
    let act = self.plan.get(i).copied().unwrap_or(Action::ReadFromInner);
    match act {
      Action::Pending => {
        // Self-wake so the caller's poll will be re-driven.
        cx.waker().wake_by_ref();
        Poll::Pending
      }
      Action::Interrupted => Poll::Ready(Err(io::Error::new(ErrorKind::Interrupted, "x"))),
      Action::OtherErr => Poll::Ready(Err(io::Error::new(ErrorKind::Other, "boom"))),
      Action::ReadFromInner => {
        let inner = std::pin::Pin::new(&mut self.inner);
        AsyncRead::poll_read(inner, cx, buf)
      }
    }
  }
}

impl AsyncWrite for FlakyReader {
  fn poll_write(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
    buf: &[u8],
  ) -> Poll<io::Result<usize>> {
    Poll::Ready(Ok(buf.len()))
  }
  fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }
  fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }
}

// ------------------------------------------------------------------
// Bug 1 regression: poll_peek does not double-put buffered data and
// does not return more bytes than were peeked.
// ------------------------------------------------------------------

#[tokio::test]
async fn bug1_poll_peek_no_double_put_on_pending() {
  // Pre-fill peek buffer with 2 bytes. Then peek 5 — first inner read
  // returns Pending. With Bug 1 the returned filled() would contain
  // the 2 buffered bytes TWICE (length 4). After the fix, length is 2.
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner, // initial fill of 2 bytes
      Action::Pending,       // peek-more attempt: returns Pending
      Action::ReadFromInner, // eventual successful read
    ],
  );
  let mut p = AsyncPeekable::from(r);

  let mut b = [0u8; 2];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"he");

  // Now peek 5 — the inner Pending should yield a partial peek of just
  // the 2 bytes we already had. Poll once manually so we can check.
  use std::future::Future;
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);

  let mut out = [0u8; 5];
  let mut buf = ReadBuf::new(&mut out);
  let pinned = Pin::new(&mut p);
  let res = pinned.poll_peek(&mut cx, &mut buf);
  let _ = res; // we don't care what variant — care about no double-put
  assert!(buf.filled().len() <= 2, "double-put: {:?}", buf.filled());
}

// ------------------------------------------------------------------
// Bug 2 regression: poll_read on Pending must not leave duplicated
// bytes in the peek buffer.
// ------------------------------------------------------------------

#[tokio::test]
async fn bug2_poll_read_on_pending_does_not_duplicate() {
  let r = FlakyReader::new(
    b"hello world".to_vec(),
    vec![
      Action::ReadFromInner, // initial fill of 4 bytes
      Action::Pending,       // first read attempt: pending
      Action::ReadFromInner, // second read attempt: succeeds
    ],
  );
  let mut p = AsyncPeekable::from(r);

  // Pre-fill peek buffer with 4 bytes.
  let mut b = [0u8; 4];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"hell");

  // Read across the Pending path, then read again to ensure bytes that
  // were already returned are not left buffered for re-emission.
  let mut out1 = vec![0u8; 8];
  let n1 = p.read(&mut out1).await.unwrap();

  let mut out2 = vec![0u8; 8];
  let n2 = p.read(&mut out2).await.unwrap();

  let mut all = Vec::with_capacity(n1 + n2);
  all.extend_from_slice(&out1[..n1]);
  all.extend_from_slice(&out2[..n2]);

  assert_eq!(
    &all,
    b"hello world",
    "duplicated or missing bytes across reads: {:?}",
    std::str::from_utf8(&all).unwrap()
  );
}

// ------------------------------------------------------------------
// Bug 3 regression: error during a read with non-empty peek buffer
// rolls back filled() instead of corrupting bytes via inner_mut().
// ------------------------------------------------------------------

#[tokio::test]
async fn bug3_read_error_rolls_back_filled() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner, // initial fill of 3 bytes
      Action::OtherErr,      // read attempt errors
    ],
  );
  let mut p = AsyncPeekable::from(r);

  let mut b = [0u8; 3];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"hel");

  // Read 5 — should hit error, and after the fix our caller's buf is
  // unchanged from the rollback.
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 5];
  let mut buf = ReadBuf::new(&mut out);
  let pinned = Pin::new(&mut p);
  match AsyncRead::poll_read(pinned, &mut cx, &mut buf) {
    Poll::Ready(Err(e)) => assert_eq!(e.kind(), ErrorKind::Other),
    other => panic!("unexpected: {:?}", other),
  }
  assert_eq!(
    buf.filled().len(),
    0,
    "filled not rolled back: {:?}",
    buf.filled()
  );
}

// ------------------------------------------------------------------
// Cover poll_peek error path (rolls back put_slice).
// ------------------------------------------------------------------

#[tokio::test]
async fn poll_peek_error_rolls_back_filled() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner, // initial fill of 2 bytes via peek
      Action::OtherErr,      // peek-more errors
    ],
  );
  let mut p = AsyncPeekable::from(r);

  let mut b = [0u8; 2];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"he");

  // Peek 5 — inner errors. Caller's buf should be unchanged.
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 5];
  let mut buf = ReadBuf::new(&mut out);
  let pinned = Pin::new(&mut p);
  match pinned.poll_peek(&mut cx, &mut buf) {
    Poll::Ready(Err(e)) => assert_eq!(e.kind(), ErrorKind::Other),
    other => panic!("unexpected: {:?}", other),
  }
  assert_eq!(buf.filled().len(), 0);
}

// ------------------------------------------------------------------
// Cover the various "happy path" branches I haven't tested.
// ------------------------------------------------------------------

#[tokio::test]
async fn read_when_peek_buffer_equals_request() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  p.peek(&mut [0u8; 4]).await.unwrap();
  let mut out = [0u8; 4];
  p.read_exact(&mut out).await.unwrap();
  assert_eq!(&out, b"abcd");
}

#[tokio::test]
async fn read_when_peek_buffer_larger_than_request() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  p.peek(&mut [0u8; 6]).await.unwrap();
  let mut out = [0u8; 2];
  p.read_exact(&mut out).await.unwrap();
  assert_eq!(&out, b"ab");
  let mut out = [0u8; 4];
  p.read_exact(&mut out).await.unwrap();
  assert_eq!(&out, b"cdef");
}

#[tokio::test]
async fn peek_to_end_then_read_to_end() {
  let mut p = Cursor::new(b"hello world".to_vec()).peekable();
  let mut out = Vec::new();
  let n = p.peek_to_end(&mut out).await.unwrap();
  assert_eq!(n, 11);

  let mut out = Vec::new();
  let n = p.read_to_end(&mut out).await.unwrap();
  assert_eq!(n, 11);
}

#[tokio::test]
async fn peek_to_string_invalid_utf8() {
  let mut p = Cursor::new(vec![0xFFu8; 4]).peekable();
  let mut s = String::new();
  let err = p.peek_to_string(&mut s).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::InvalidData);
}

#[tokio::test]
async fn peek_exact_eof_error() {
  let mut p = Cursor::new(b"ab".to_vec()).peekable();
  let mut buf = [0u8; 5];
  let err = p.peek_exact(&mut buf).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
}

#[tokio::test]
async fn peek_into_zero_length_buffer() {
  let mut p = Cursor::new(b"abc".to_vec()).peekable();
  let mut buf = [];
  let _ = p.peek(&mut buf).await.unwrap();
}

// ------------------------------------------------------------------
// peek_buf coverage — completely uncovered before.
// ------------------------------------------------------------------

#[tokio::test]
async fn peek_buf_basic() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  let mut bytes_buf = BytesMut::with_capacity(4);
  let n = p.peek_buf(&mut bytes_buf).await.unwrap();
  assert_eq!(n, 4);
  assert_eq!(&bytes_buf[..], b"abcd");
}

#[tokio::test]
async fn peek_buf_into_empty_slice_returns_zero() {
  // `&mut [u8]` has a finite remaining_mut(), unlike BytesMut which
  // is unbounded. Empty slice exercises the `!has_remaining_mut` path.
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  let mut empty: [u8; 0] = [];
  let mut s: &mut [u8] = &mut empty;
  let n = p.peek_buf(&mut s).await.unwrap();
  assert_eq!(n, 0);
}

// ------------------------------------------------------------------
// fill_peek_buf coverage including new error/pending rollback.
// ------------------------------------------------------------------

#[tokio::test]
async fn fill_peek_buf_basic() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable_with_capacity(8);
  let n = p.fill_peek_buf().await.unwrap();
  assert_eq!(n, 4);
}

#[tokio::test]
async fn fill_peek_buf_when_already_at_capacity_returns_zero() {
  // Pre-fill the peek buffer to capacity. Use Vec backend so capacity()
  // returns exactly the requested cap (SmallVec has 64-byte inline
  // capacity that masks this).
  let mut p: AsyncPeekable<_, Vec<u8>> =
    AsyncPeekable::with_capacity_and_buffer(Cursor::new(b"abcdefgh".to_vec()), 4);
  p.peek(&mut [0u8; 4]).await.unwrap();
  let n = p.fill_peek_buf().await.unwrap();
  assert_eq!(n, 0);
}

#[tokio::test]
async fn fill_peek_buf_error_truncates_buffer() {
  let r = FlakyReader::new(b"abcd".to_vec(), vec![Action::OtherErr]);
  let mut p = AsyncPeekable::with_capacity(r, 8);
  let err = p.fill_peek_buf().await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
  // Subsequent successful peek should not see ghost zero-bytes.
  let mut buf = [0u8; 4];
  let n = p.peek(&mut buf).await.unwrap();
  assert_eq!(n, 4);
  assert_eq!(&buf, b"abcd");
}

// ------------------------------------------------------------------
// API surface (consume, get_mut, get_ref, into_components, From, etc).
// ------------------------------------------------------------------

#[tokio::test]
async fn from_tuple_constructor() {
  let p: AsyncPeekable<_> = AsyncPeekable::from((16, Cursor::new(b"abc".to_vec())));
  let _ = p.into_components();
}

#[tokio::test]
async fn with_buffer_vec() {
  let mut p: AsyncPeekable<_, Vec<u8>> = Cursor::new(b"abc".to_vec()).peekable_with_buffer();
  let mut b = [0u8; 3];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"abc");
}

#[tokio::test]
async fn with_capacity_and_buffer_vec() {
  let mut p: AsyncPeekable<_, Vec<u8>> =
    Cursor::new(b"abc".to_vec()).peekable_with_capacity_and_buffer(64);
  let mut b = [0u8; 3];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"abc");
}

#[cfg(feature = "tinyvec")]
#[tokio::test]
async fn with_buffer_tinyvec() {
  let mut p: AsyncPeekable<_, tinyvec::TinyVec<[u8; 16]>> =
    Cursor::new(b"abc".to_vec()).peekable_with_buffer();
  let mut b = [0u8; 3];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"abc");
}

#[tokio::test]
async fn consume_in_place_and_consume_with_capacity() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable_with_capacity(64);
  p.peek(&mut [0u8; 2]).await.unwrap();
  let buf = p.consume();
  assert!(peekable::buffer::Buffer::capacity(&buf) >= 64);

  p.peek(&mut [0u8; 2]).await.unwrap();
  p.consume_in_place();
  assert_eq!(peekable::buffer::Buffer::len(&p.consume()), 0);
}

#[tokio::test]
async fn get_ref_and_get_mut() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  p.peek(&mut [0u8; 2]).await.unwrap();
  let (peeked, _) = p.get_ref();
  assert_eq!(peeked, b"ab");
  let (peeked, _) = p.get_mut();
  assert_eq!(peeked, b"ab");
}

// ------------------------------------------------------------------
// AsyncWrite delegation.
// ------------------------------------------------------------------

#[tokio::test]
async fn async_write_delegates() {
  use ::tokio::io::AsyncWriteExt;
  let r = FlakyReader::new(b"".to_vec(), vec![]);
  let mut p = AsyncPeekable::from(r);
  p.write_all(b"hi").await.unwrap();
  p.flush().await.unwrap();
  p.shutdown().await.unwrap();
}

// ------------------------------------------------------------------
// Error paths in helper futures (peek_to_end, peek_to_string).
// ------------------------------------------------------------------

#[tokio::test]
async fn peek_to_end_propagates_error() {
  let r = FlakyReader::new(
    b"abc".to_vec(),
    vec![
      Action::ReadFromInner, // initial read
      Action::OtherErr,      // subsequent read errors
    ],
  );
  let mut p = AsyncPeekable::from(r);
  let mut out = Vec::new();
  let err = p.peek_to_end(&mut out).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

#[tokio::test]
async fn peek_to_string_propagates_error() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner, // first read returns "hello"
      Action::OtherErr,      // next read errors
    ],
  );
  let mut p = AsyncPeekable::from(r);
  let mut s = String::new();
  let err = p.peek_to_string(&mut s).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

// ------------------------------------------------------------------
// Trait deref impls (Box<dyn>, &mut, Pin<P>).
// ------------------------------------------------------------------

#[tokio::test]
async fn async_peek_through_box_dyn() {
  let p = AsyncPeekable::from(Cursor::new(b"abc".to_vec()));
  let mut boxed: Box<dyn AsyncPeek + Unpin> = Box::new(p);
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 3];
  let mut buf = ReadBuf::new(&mut out);
  match Pin::new(&mut boxed).poll_peek(&mut cx, &mut buf) {
    Poll::Ready(Ok(())) => {}
    other => panic!("unexpected: {:?}", other),
  }
  assert_eq!(buf.filled(), b"abc");
}

#[tokio::test]
async fn async_peek_through_mut_ref() {
  let mut p = AsyncPeekable::from(Cursor::new(b"abc".to_vec()));
  let mut peek_ref: &mut (dyn AsyncPeek + Unpin) = &mut p;
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 3];
  let mut buf = ReadBuf::new(&mut out);
  match Pin::new(&mut peek_ref).poll_peek(&mut cx, &mut buf) {
    Poll::Ready(Ok(())) => {}
    other => panic!("unexpected: {:?}", other),
  }
}

#[tokio::test]
async fn async_peek_through_pin() {
  let p = AsyncPeekable::from(Cursor::new(b"abc".to_vec()));
  let mut pinned: Pin<Box<AsyncPeekable<Cursor<Vec<u8>>>>> = Box::pin(p);
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 3];
  let mut buf = ReadBuf::new(&mut out);
  match Pin::new(&mut pinned).poll_peek(&mut cx, &mut buf) {
    Poll::Ready(Ok(())) => {}
    other => panic!("unexpected: {:?}", other),
  }
}

// ------------------------------------------------------------------
// poll_read direct error / pending paths in the no-buffer branch
// (where we just delegate to the inner reader).
// ------------------------------------------------------------------

#[tokio::test]
async fn poll_read_no_buffer_inner_error() {
  let r = FlakyReader::new(b"".to_vec(), vec![Action::OtherErr]);
  let mut p = AsyncPeekable::from(r);
  let mut out = [0u8; 4];
  let err = p.read(&mut out).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

#[tokio::test]
async fn poll_peek_no_buffer_inner_error() {
  let r = FlakyReader::new(b"".to_vec(), vec![Action::OtherErr]);
  let mut p = AsyncPeekable::from(r);
  let mut out = [0u8; 4];
  let err = p.peek(&mut out).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

// ------------------------------------------------------------------
// Manual-poll Pending paths.
// ------------------------------------------------------------------

#[tokio::test]
async fn fill_peek_buf_pending_truncates_buffer() {
  use std::future::Future;

  let r = FlakyReader::new(b"".to_vec(), vec![Action::Pending]);
  let mut p = AsyncPeekable::with_capacity(r, 8);

  let mut fut = Box::pin(p.fill_peek_buf());
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  match fut.as_mut().poll(&mut cx) {
    Poll::Pending => {} // expected
    other => panic!("unexpected: {:?}", other),
  }
  drop(fut);
  // After the Pending poll, the buffer must have been rolled back to
  // length 0.
  assert_eq!(peekable::buffer::Buffer::len(&p.consume()), 0);
}
