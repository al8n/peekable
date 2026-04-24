//! Integration tests for the tokio `AsyncPeekable`.
//!
//! Includes regression tests for bugs found in the deep review:
//! - Bug 1: `poll_peek` no longer double-puts buffered data on Pending.
//! - Bug 2: `poll_read` no longer leaks buffered data on Pending nor
//!   leaves it for re-emission on a later call.
//! - Bug 3: error path no longer mis-indexes into `buf.inner_mut()`.

#![cfg(feature = "tokio")]
#![allow(warnings)]

use std::{
  io::{self, Cursor, ErrorKind},
  pin::Pin,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
  task::{Context, Poll},
};

use ::tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use bytes::BytesMut;
use peekable::tokio::{AsyncPeek, AsyncPeekExt, AsyncPeekable};

// ---- Flaky reader -------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Action {
  Pending, // return Pending without registering a wake (test will explicit-wake)
  Interrupted,
  WouldBlock,
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
      Action::WouldBlock => Poll::Ready(Err(io::Error::new(ErrorKind::WouldBlock, "wb"))),
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
// poll_read with non-empty peek buffer + inner error: the AsyncRead
// contract forbids returning Err after writing bytes, so the buffered
// bytes are delivered to the caller's ReadBuf and the inner error
// surfaces on a subsequent poll_read once the peek buffer is empty.
// ------------------------------------------------------------------

#[tokio::test]
async fn read_delivers_buffered_bytes_then_surfaces_inner_err() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner, // initial fill of 3 bytes
      Action::OtherErr,      // top-up errors
      Action::OtherErr,      // next direct read also errors
    ],
  );
  let mut p = AsyncPeekable::from(r);

  let mut b = [0u8; 3];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"hel");

  // First poll_read(5): we have 3 buffered bytes, inner errors on
  // top-up. Expect Ok(()) with 3 bytes filled from the peek buffer.
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 5];
  let mut buf = ReadBuf::new(&mut out);
  let pinned = Pin::new(&mut p);
  match AsyncRead::poll_read(pinned, &mut cx, &mut buf) {
    Poll::Ready(Ok(())) => {
      assert_eq!(buf.filled(), b"hel");
    }
    other => panic!("unexpected: {:?}", other),
  }

  // Second poll_read: peek buffer is now empty, delegates to inner,
  // which returns Other. Caller sees the error.
  let mut out = [0u8; 5];
  let mut buf = ReadBuf::new(&mut out);
  let pinned = Pin::new(&mut p);
  match AsyncRead::poll_read(pinned, &mut cx, &mut buf) {
    Poll::Ready(Err(e)) => assert_eq!(e.kind(), ErrorKind::Other),
    other => panic!("unexpected: {:?}", other),
  }
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

// ------------------------------------------------------------------
// Regression: tokio peek_to_end must not duplicate the prefix on
// re-poll after Pending. Mirrors the futures_io.rs test.
// ------------------------------------------------------------------

#[tokio::test]
async fn peek_to_end_survives_pending_boundary() {
  let r = FlakyReader::new(
    b"abcdef".to_vec(),
    vec![
      Action::ReadFromInner, // fills peek buffer with first chunk
      Action::Pending,       // forces a re-poll
      Action::ReadFromInner, // remainder
    ],
  );
  let mut p = AsyncPeekable::new(r);

  // Pre-fill a couple bytes so the peek buffer has a prefix.
  let mut pre = [0u8; 2];
  p.peek(&mut pre).await.unwrap();
  assert_eq!(&pre, b"ab");

  // peek_to_end must return all bytes exactly once.
  let mut out = Vec::new();
  let n = p.peek_to_end(&mut out).await.unwrap();
  assert_eq!(n, 6);
  assert_eq!(&out, b"abcdef");
}

// ------------------------------------------------------------------
// Regression: tokio peek_to_string with a peek buffer ending
// mid-codepoint must not spuriously reject the stream.
// ------------------------------------------------------------------

#[tokio::test]
async fn peek_to_string_with_mid_codepoint_peek_buffer() {
  // "é" = [0xC3, 0xA9]. Peek 2 bytes: 'h' + first byte of 'é'.
  let data = "héllo".as_bytes().to_vec();
  let r = FlakyReader::new(data, vec![Action::ReadFromInner]);
  let mut p = AsyncPeekable::new(r);

  // Peek buffer ends with an incomplete UTF-8 sequence.
  let mut pre = [0u8; 2];
  p.peek_exact(&mut pre).await.unwrap();
  assert_eq!(&pre, &[0x68, 0xC3]);

  // peek_to_string must complete the sequence, not reject it.
  let mut s = String::new();
  let n = p.peek_to_string(&mut s).await.unwrap();
  assert_eq!(s, "héllo");
  assert_eq!(n, "héllo".len());
}

#[tokio::test]
async fn peek_to_end_keeps_partial_data_on_error() {
  struct ErrorAfterFirstRead {
    state: u8,
  }

  impl AsyncRead for ErrorAfterFirstRead {
    fn poll_read(
      mut self: Pin<&mut Self>,
      _cx: &mut Context<'_>,
      buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
      match self.state {
        0 => {
          buf.put_slice(b"ab");
          self.state = 1;
          Poll::Ready(Ok(()))
        }
        _ => Poll::Ready(Err(io::Error::new(ErrorKind::Other, "boom"))),
      }
    }
  }

  let r = ErrorAfterFirstRead { state: 0 };
  let mut p = AsyncPeekable::new(r);

  let mut pre = [0u8; 2];
  p.peek_exact(&mut pre).await.unwrap();
  assert_eq!(&pre, b"ab");

  let mut out = b"seed".to_vec();
  let err = p.peek_to_end(&mut out).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);

  // On error, partial data stays in buf — matching std/tokio's
  // read_to_end contract. The peek prefix "ab" was appended.
  assert!(out.starts_with(b"seed"));
  assert!(out.len() > 4); // "seed" + at least the peek prefix
}

// ------------------------------------------------------------------
// Regression tests for:
//   Finding 1: peek_to_string must preserve partial valid-UTF-8 in
//              the caller's String on ordinary I/O errors.
//   Finding 2: a fallible Buffer must not leave the inner reader
//              advanced past what the peek buffer can replay.
// ------------------------------------------------------------------

use peekable::buffer::Buffer;

#[derive(Debug, Default)]
struct BoundedBuffer<const CAP: usize> {
  inner: Vec<u8>,
}

impl<const CAP: usize> AsRef<[u8]> for BoundedBuffer<CAP> {
  fn as_ref(&self) -> &[u8] {
    &self.inner
  }
}

impl<const CAP: usize> Buffer for BoundedBuffer<CAP> {
  fn new() -> Self {
    Self { inner: Vec::new() }
  }
  fn with_capacity(_: usize) -> Self {
    Self { inner: Vec::new() }
  }
  fn consume(&mut self, rng: std::ops::RangeTo<usize>) {
    self.inner.drain(rng);
  }
  fn clear(&mut self) {
    self.inner.clear();
  }
  fn resize(&mut self, len: usize) -> io::Result<()> {
    if len > CAP {
      return Err(io::Error::new(
        io::ErrorKind::OutOfMemory,
        "BoundedBuffer cap",
      ));
    }
    self.inner.resize(len, 0);
    Ok(())
  }
  fn truncate(&mut self, len: usize) {
    self.inner.truncate(len);
  }
  fn extend_from_slice(&mut self, other: &[u8]) -> io::Result<()> {
    if self.inner.len() + other.len() > CAP {
      return Err(io::Error::new(
        io::ErrorKind::OutOfMemory,
        "BoundedBuffer cap",
      ));
    }
    self.inner.extend_from_slice(other);
    Ok(())
  }
  fn as_slice(&self) -> &[u8] {
    &self.inner
  }
  fn as_mut_slice(&mut self) -> &mut [u8] {
    &mut self.inner
  }
  fn len(&self) -> usize {
    self.inner.len()
  }
  fn is_empty(&self) -> bool {
    self.inner.is_empty()
  }
  fn capacity(&self) -> usize {
    CAP
  }
}

struct AsyncErroringReader {
  data: Cursor<Vec<u8>>,
  error_after: usize,
  served: usize,
  kind: ErrorKind,
}

impl AsyncErroringReader {
  fn new(data: Vec<u8>, error_after: usize, kind: ErrorKind) -> Self {
    Self {
      data: Cursor::new(data),
      error_after,
      served: 0,
      kind,
    }
  }
}

impl AsyncRead for AsyncErroringReader {
  fn poll_read(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    let me = self.get_mut();
    if me.served >= me.error_after {
      return Poll::Ready(Err(io::Error::new(me.kind, "synthetic")));
    }
    let room = (me.error_after - me.served).min(buf.remaining());
    if room == 0 {
      return Poll::Ready(Ok(()));
    }
    let start = me.data.position() as usize;
    let end = (start + room).min(me.data.get_ref().len());
    let len = end - start;
    let slice = me.data.get_ref()[start..end].to_vec();
    buf.put_slice(&slice);
    me.data.set_position(end as u64);
    me.served += len;
    Poll::Ready(Ok(()))
  }
}

#[tokio::test]
async fn tokio_peek_does_not_advance_reader_when_buffer_extend_fails() {
  let reader = Cursor::new(vec![1u8, 2, 3, 4]);
  let mut p: AsyncPeekable<_, BoundedBuffer<0>> = reader.peekable_with_buffer();
  let mut buf = [0u8; 2];
  let err = p.peek(&mut buf).await.expect_err("must fail");
  assert_eq!(err.kind(), ErrorKind::OutOfMemory);
  let (_, reader) = p.get_mut();
  let mut out = Vec::new();
  reader.read_to_end(&mut out).await.unwrap();
  assert_eq!(out, [1, 2, 3, 4]);
}

#[tokio::test]
async fn tokio_peek_exact_does_not_advance_reader_when_buffer_extend_fails() {
  let reader = Cursor::new(vec![1u8, 2, 3, 4]);
  let mut p: AsyncPeekable<_, BoundedBuffer<0>> = reader.peekable_with_buffer();
  let mut buf = [0u8; 4];
  let err = p.peek_exact(&mut buf).await.expect_err("must fail");
  assert_eq!(err.kind(), ErrorKind::OutOfMemory);
  let (peek_slice, reader) = p.get_mut();
  let have = peek_slice.len();
  let mut out = Vec::new();
  reader.read_to_end(&mut out).await.unwrap();
  assert_eq!(have + out.len(), 4);
}

#[tokio::test]
async fn tokio_peek_to_end_preserves_partial_data_on_io_error() {
  let reader = AsyncErroringReader::new(b"hello".to_vec(), 3, ErrorKind::ConnectionReset);
  let mut p = reader.peekable();
  let mut out = Vec::new();
  let err = p.peek_to_end(&mut out).await.expect_err("io error");
  assert_eq!(err.kind(), ErrorKind::ConnectionReset);
  assert_eq!(out, b"hel");
}

#[tokio::test]
async fn tokio_peek_to_end_does_not_desync_on_buffer_error() {
  let reader = Cursor::new(b"abcdef".to_vec());
  let mut p: AsyncPeekable<_, BoundedBuffer<2>> = reader.peekable_with_buffer();
  let mut scratch = [0u8; 2];
  assert_eq!(p.peek(&mut scratch).await.unwrap(), 2);
  let mut out = Vec::new();
  let err = p.peek_to_end(&mut out).await.expect_err("must fail");
  assert_eq!(err.kind(), ErrorKind::OutOfMemory);
  let (peek_slice, reader) = p.get_mut();
  let have = peek_slice.len();
  let mut tail = Vec::new();
  reader.read_to_end(&mut tail).await.unwrap();
  assert_eq!(have + tail.len(), 6);
}

#[tokio::test]
async fn tokio_peek_to_string_preserves_partial_valid_utf8_on_io_error() {
  let reader = AsyncErroringReader::new(b"hello".to_vec(), 3, ErrorKind::ConnectionReset);
  let mut p = reader.peekable();
  let mut out = String::new();
  let err = p.peek_to_string(&mut out).await.expect_err("io error");
  assert_eq!(err.kind(), ErrorKind::ConnectionReset);
  assert_eq!(out, "hel");
}

#[tokio::test]
async fn tokio_peek_to_string_leaves_buf_unchanged_when_partial_bytes_are_invalid_utf8() {
  let reader = AsyncErroringReader::new(vec![0xE2, 0x82, 0xAC], 2, ErrorKind::ConnectionReset);
  let mut p = reader.peekable();
  let mut out = String::from("prefix:");
  let err = p.peek_to_string(&mut out).await.expect_err("io error");
  assert_eq!(err.kind(), ErrorKind::ConnectionReset);
  assert_eq!(out, "prefix:");
}

#[tokio::test]
async fn tokio_peek_to_string_returns_invalid_data_for_clean_invalid_utf8() {
  let reader = Cursor::new(vec![0xFF, 0xFE]);
  let mut p = reader.peekable();
  let mut out = String::from("prefix:");
  let err = p.peek_to_string(&mut out).await.expect_err("must fail");
  assert_eq!(err.kind(), ErrorKind::InvalidData);
  assert_eq!(out, "prefix:");
}

// ---- Coverage for Interrupted / Pending / Err paths -----------------

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_exact_retries_on_interrupted() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![Action::Interrupted, Action::ReadFromInner],
  );
  let mut p = AsyncPeekable::from(r);
  let mut b = [0u8; 5];
  p.peek_exact(&mut b).await.unwrap();
  assert_eq!(&b, b"hello");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_exact_propagates_error() {
  let r = FlakyReader::new(b"hello".to_vec(), vec![Action::OtherErr]);
  let mut p = AsyncPeekable::from(r);
  let mut b = [0u8; 5];
  let err = p.peek_exact(&mut b).await.unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_exact_survives_pending_boundary() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![Action::Pending, Action::ReadFromInner],
  );
  let mut p = AsyncPeekable::from(r);
  let mut b = [0u8; 5];
  p.peek_exact(&mut b).await.unwrap();
  assert_eq!(&b, b"hello");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_end_retries_on_interrupted() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner,
      Action::Interrupted,
      Action::ReadFromInner,
    ],
  );
  let mut p = AsyncPeekable::from(r);
  let mut out = Vec::new();
  let n = p.peek_to_end(&mut out).await.unwrap();
  assert_eq!(out, b"hello");
  assert_eq!(n, 5);
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_end_survives_pending() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner,
      Action::Pending,
      Action::ReadFromInner,
    ],
  );
  let mut p = AsyncPeekable::from(r);
  let mut out = Vec::new();
  let n = p.peek_to_end(&mut out).await.unwrap();
  assert_eq!(out, b"hello");
  assert_eq!(n, 5);
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_string_retries_on_interrupted() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner,
      Action::Interrupted,
      Action::ReadFromInner,
    ],
  );
  let mut p = AsyncPeekable::from(r);
  let mut out = String::new();
  let n = p.peek_to_string(&mut out).await.unwrap();
  assert_eq!(out, "hello");
  assert_eq!(n, 5);
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_string_survives_pending() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Action::ReadFromInner,
      Action::Pending,
      Action::ReadFromInner,
    ],
  );
  let mut p = AsyncPeekable::from(r);
  let mut out = String::new();
  let n = p.peek_to_string(&mut out).await.unwrap();
  assert_eq!(out, "hello");
  assert_eq!(n, 5);
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_string_buffer_resize_fails_preserves_partial_utf8() {
  let reader = Cursor::new(b"hello".to_vec());
  let mut p: AsyncPeekable<_, BoundedBuffer<2>> = reader.peekable_with_capacity_and_buffer(2);
  let mut scratch = [0u8; 2];
  assert_eq!(p.peek(&mut scratch).await.unwrap(), 2);
  let mut out = String::from("prefix:");
  let err = p.peek_to_string(&mut out).await.expect_err("must fail");
  assert_eq!(err.kind(), ErrorKind::OutOfMemory);
  assert_eq!(out, "prefix:he");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_end_succeeds_with_small_cap_when_data_fits() {
  let reader = Cursor::new(b"0123456789".to_vec());
  let mut p: AsyncPeekable<_, BoundedBuffer<100>> = reader.peekable_with_buffer();
  let mut out = Vec::new();
  let n = p.peek_to_end(&mut out).await.unwrap();
  assert_eq!(n, 10);
  assert_eq!(out, b"0123456789");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_string_succeeds_with_small_cap_when_data_fits() {
  let reader = Cursor::new(b"hello".to_vec());
  let mut p: AsyncPeekable<_, BoundedBuffer<100>> = reader.peekable_with_buffer();
  let mut out = String::new();
  let n = p.peek_to_string(&mut out).await.unwrap();
  assert_eq!(n, 5);
  assert_eq!(out, "hello");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_to_string_preserves_valid_prefix_before_incomplete_utf8_on_io_error() {
  let reader = AsyncErroringReader::new(vec![b'h', 0xE2, 0x82], 3, ErrorKind::ConnectionReset);
  let mut p = reader.peekable();
  let mut out = String::from("prefix:");
  let err = p.peek_to_string(&mut out).await.expect_err("io error");
  assert_eq!(err.kind(), ErrorKind::ConnectionReset);
  assert_eq!(out, "prefix:h");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_retries_interrupted_from_inner() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![Action::Interrupted, Action::ReadFromInner],
  );
  let mut p = AsyncPeekable::from(r);
  let mut b = [0u8; 5];
  let n = p.peek(&mut b).await.unwrap();
  assert_eq!(n, 5);
  assert_eq!(&b, b"hello");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_converts_wouldblock_to_pending_no_buffer() {
  use std::future::Future;
  let r = FlakyReader::new(b"hello".to_vec(), vec![Action::WouldBlock]);
  let mut p = AsyncPeekable::from(r);
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 5];
  let mut fut = Box::pin(p.peek(&mut out));
  match fut.as_mut().poll(&mut cx) {
    Poll::Pending => {} // expected
    other => panic!("unexpected: {:?}", other),
  }
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_peek_converts_wouldblock_to_partial_with_buffer() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![Action::ReadFromInner, Action::WouldBlock],
  );
  let mut p = AsyncPeekable::from(r);
  let mut b = [0u8; 2];
  p.peek(&mut b).await.unwrap();
  assert_eq!(&b, b"he");

  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 5];
  let mut buf = ReadBuf::new(&mut out);
  let pinned = Pin::new(&mut p);
  match pinned.poll_peek(&mut cx, &mut buf) {
    Poll::Ready(Ok(())) => {
      assert_eq!(buf.filled(), b"he");
    }
    other => panic!("unexpected: {:?}", other),
  }
}
