//! Integration tests for the futures-util `AsyncPeekable`.
//!
//! Includes regression coverage for the bug-fix scenarios and the
//! previously-untested code paths (peek_vectored, fill_peek_buf
//! truncation on Pending/Err, etc.).

#![cfg(feature = "future")]
#![allow(warnings)]

use futures::io::Cursor;
use std::{
  io::{self, ErrorKind},
  pin::Pin,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
  task::{Context, Poll},
};

use futures::io::{AsyncReadExt, IoSliceMut};
use futures_util::{AsyncRead, AsyncWrite};
use peekable::future::{AsyncPeek, AsyncPeekExt, AsyncPeekable};

// ---- Flaky reader -------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Action {
  Pending,
  Interrupted,
  OtherErr,
  ReadFromInner,
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
}

impl AsyncRead for FlakyReader {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut [u8],
  ) -> Poll<io::Result<usize>> {
    let i = self.call.fetch_add(1, Ordering::SeqCst);
    let act = self.plan.get(i).copied().unwrap_or(Action::ReadFromInner);
    match act {
      Action::Pending => {
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
  fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }
}

// ------------------------------------------------------------------
// Bug 7 regression: poll_peek error in the Greater branch must roll
// back the resize so the buffer doesn't carry ghost zero-bytes.
// ------------------------------------------------------------------

#[test]
fn bug7_poll_peek_error_truncates_buffer() {
  futures::executor::block_on(async {
    let r = FlakyReader::new(
      b"hello".to_vec(),
      vec![
        Action::ReadFromInner, // initial fill of 2
        Action::OtherErr,      // peek-more errors
      ],
    );
    let mut p = AsyncPeekable::new(r);
    let mut b = [0u8; 2];
    p.peek(&mut b).await.unwrap();
    assert_eq!(&b, b"he");

    let mut b = [0u8; 5];
    let err = p.peek(&mut b).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);

    // Subsequent peek must not return zero-bytes from the rolled-back
    // resize.
    let mut b = [0u8; 2];
    let n = p.peek(&mut b).await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(&b, b"he");
  });
}

#[test]
fn bug7_poll_peek_pending_truncates_buffer() {
  futures::executor::block_on(async {
    // Pre-fill 2 bytes, then peek 5: inner reader returns Pending,
    // then succeeds. The Pending branch must truncate so the next
    // call doesn't see ghost zero-bytes.
    let r = FlakyReader::new(
      b"hello".to_vec(),
      vec![
        Action::ReadFromInner, // fill 2
        Action::Pending,
        Action::ReadFromInner, // succeed
      ],
    );
    let mut p = AsyncPeekable::new(r);
    let mut b = [0u8; 2];
    p.peek(&mut b).await.unwrap();
    assert_eq!(&b, b"he");

    let mut b = [0u8; 5];
    // The await will re-poll until ready. After the (now-truncated)
    // Pending branch the second poll will succeed.
    let n = p.peek(&mut b).await.unwrap();
    // We accept either a partial peek (2) or full peek (5) — what
    // matters is that we don't see ghost zero-bytes.
    let valid = match n {
      2 => &b[..2] == b"he",
      5 => &b[..] == b"hello",
      _ => false,
    };
    assert!(valid, "unexpected peek result: n={} b={:?}", n, b);
  });
}

// ------------------------------------------------------------------
// poll_peek `Ordering::Less` branch: subsequent peek requests fewer
// bytes than the peek buffer already holds. Must be served entirely
// from the buffer without touching the inner reader.
// ------------------------------------------------------------------

#[test]
fn poll_peek_want_less_than_buffer() {
  futures::executor::block_on(async {
    // The inner reader's plan is empty after the first fill — if the
    // second, smaller peek ever touches the inner reader, it will
    // default to `ReadFromInner` and eventually EOF, but we assert
    // that the returned slice matches the buffer prefix exactly.
    let r = FlakyReader::new(
      b"abcdef".to_vec(),
      vec![Action::ReadFromInner], // only the first peek hits the reader
    );
    let mut p = AsyncPeekable::new(r);

    // Fill the peek buffer with 4 bytes.
    let mut b = [0u8; 4];
    p.peek(&mut b).await.unwrap();
    assert_eq!(&b, b"abcd");
    let calls_after_first = p.get_ref().1.call.load(Ordering::SeqCst);

    // Ask for fewer bytes than the buffer already holds.
    let mut b = [0u8; 2];
    let n = p.peek(&mut b).await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(&b, b"ab");

    // The inner reader must not have been polled again.
    let calls_after_second = p.get_ref().1.call.load(Ordering::SeqCst);
    assert_eq!(
      calls_after_first, calls_after_second,
      "Ordering::Less branch must not touch the inner reader"
    );

    // The peek buffer still contains the original 4 bytes.
    let mut b = [0u8; 4];
    let n = p.peek(&mut b).await.unwrap();
    assert_eq!(n, 4);
    assert_eq!(&b, b"abcd");
  });
}

// ------------------------------------------------------------------
// fill_peek_buf rollback on Pending and Err.
// ------------------------------------------------------------------

#[test]
fn fill_peek_buf_error_truncates_buffer() {
  futures::executor::block_on(async {
    let r = FlakyReader::new(b"abcd".to_vec(), vec![Action::OtherErr]);
    let mut p = AsyncPeekable::with_capacity(r, 8);
    let err = p.fill_peek_buf().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);

    let mut buf = [0u8; 4];
    let n = p.peek(&mut buf).await.unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"abcd");
  });
}

// ------------------------------------------------------------------
// Coverage for previously-untested paths.
// ------------------------------------------------------------------

#[test]
fn peek_to_end_then_read_to_end() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"hello world".to_vec()).peekable();
    let mut out = Vec::new();
    let n = p.peek_to_end(&mut out).await.unwrap();
    assert_eq!(n, 11);

    let mut out = Vec::new();
    let n = p.read_to_end(&mut out).await.unwrap();
    assert_eq!(n, 11);
  });
}

#[test]
fn peek_to_string_invalid_utf8() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(vec![0xFFu8; 4]).peekable();
    let mut s = String::new();
    let err = p.peek_to_string(&mut s).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData);
  });
}

#[test]
fn peek_to_string_invalid_utf8_in_buffer() {
  futures::executor::block_on(async {
    // Pre-fill peek buffer with invalid UTF-8 bytes.
    let mut p = Cursor::new(b"abc\xff".to_vec()).peekable();
    let mut buf = [0u8; 4];
    p.peek(&mut buf).await.unwrap();
    let mut s = String::new();
    let err = p.peek_to_string(&mut s).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData);
  });
}

#[test]
fn peek_exact_eof_error() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"ab".to_vec()).peekable();
    let mut buf = [0u8; 5];
    let err = p.peek_exact(&mut buf).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
  });
}

#[test]
fn peek_into_zero_length_buffer() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abc".to_vec()).peekable();
    let mut buf = [];
    let _ = p.peek(&mut buf).await.unwrap();
  });
}

// ------------------------------------------------------------------
// peek_vectored — completely uncovered before.
// ------------------------------------------------------------------

#[test]
fn peek_vectored_basic() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
    let mut a = [0u8; 3];
    let mut b = [0u8; 3];
    let mut bufs = [IoSliceMut::new(&mut a), IoSliceMut::new(&mut b)];
    let n = p.peek_vectored(&mut bufs).await.unwrap();
    assert!(n > 0);
    // Default impl peeks into the first non-empty buf.
    assert_eq!(&a[..3], b"abc");
  });
}

#[test]
fn peek_vectored_empty_first_buf() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
    let mut a = [];
    let mut b = [0u8; 3];
    let mut bufs = [IoSliceMut::new(&mut a), IoSliceMut::new(&mut b)];
    let n = p.peek_vectored(&mut bufs).await.unwrap();
    assert_eq!(n, 3);
    assert_eq!(&b[..3], b"abc");
  });
}

#[test]
fn peek_vectored_all_empty() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
    let mut bufs: [IoSliceMut<'_>; 0] = [];
    let n = p.peek_vectored(&mut bufs).await.unwrap();
    assert_eq!(n, 0);
  });
}

// ------------------------------------------------------------------
// AsyncRead Equal/Less branches and Greater fully covered.
// ------------------------------------------------------------------

#[test]
fn read_when_peek_buffer_equals_request() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcd".to_vec()).peekable();
    p.peek(&mut [0u8; 4]).await.unwrap();
    let mut out = [0u8; 4];
    p.read_exact(&mut out).await.unwrap();
    assert_eq!(&out, b"abcd");
  });
}

#[test]
fn read_when_peek_buffer_larger_than_request() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
    p.peek(&mut [0u8; 6]).await.unwrap();
    let mut out = [0u8; 2];
    p.read_exact(&mut out).await.unwrap();
    assert_eq!(&out, b"ab");
    let mut out = [0u8; 4];
    p.read_exact(&mut out).await.unwrap();
    assert_eq!(&out, b"cdef");
  });
}

// ------------------------------------------------------------------
// API surface coverage.
// ------------------------------------------------------------------

#[test]
fn from_tuple_constructor() {
  futures::executor::block_on(async {
    let p: AsyncPeekable<_> = AsyncPeekable::from((16, Cursor::new(b"abc".to_vec())));
    let _ = p.into_components();
  });
}

#[test]
fn with_buffer_vec() {
  futures::executor::block_on(async {
    let mut p: AsyncPeekable<_, Vec<u8>> = Cursor::new(b"abc".to_vec()).peekable_with_buffer();
    let mut b = [0u8; 3];
    p.peek(&mut b).await.unwrap();
    assert_eq!(&b, b"abc");
  });
}

#[test]
fn with_capacity_and_buffer_vec() {
  futures::executor::block_on(async {
    let mut p: AsyncPeekable<_, Vec<u8>> =
      Cursor::new(b"abc".to_vec()).peekable_with_capacity_and_buffer(64);
    let mut b = [0u8; 3];
    p.peek(&mut b).await.unwrap();
    assert_eq!(&b, b"abc");
  });
}

#[cfg(feature = "tinyvec")]
#[test]
fn with_buffer_tinyvec() {
  futures::executor::block_on(async {
    let mut p: AsyncPeekable<_, tinyvec::TinyVec<[u8; 16]>> =
      Cursor::new(b"abc".to_vec()).peekable_with_buffer();
    let mut b = [0u8; 3];
    p.peek(&mut b).await.unwrap();
    assert_eq!(&b, b"abc");
  });
}

#[test]
fn consume_and_consume_in_place() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcdefgh".to_vec()).peekable_with_capacity(64);
    p.peek(&mut [0u8; 2]).await.unwrap();
    let buf = p.consume();
    assert_eq!(peekable::buffer::Buffer::as_slice(&buf), b"ab");

    p.peek(&mut [0u8; 2]).await.unwrap();
    p.consume_in_place();
    let mut out = [0u8; 2];
    let n = p.peek(&mut out).await.unwrap();
    assert_eq!(n, 2);
  });
}

#[test]
fn get_ref_and_get_mut() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcd".to_vec()).peekable();
    p.peek(&mut [0u8; 2]).await.unwrap();
    let (peeked, _) = p.get_ref();
    assert_eq!(peeked, b"ab");
    let (peeked, _) = p.get_mut();
    assert_eq!(peeked, b"ab");
  });
}

#[test]
fn fill_peek_buf_basic() {
  futures::executor::block_on(async {
    let mut p = Cursor::new(b"abcd".to_vec()).peekable_with_capacity(8);
    let n = p.fill_peek_buf().await.unwrap();
    assert_eq!(n, 4);
  });
}

#[test]
fn fill_peek_buf_already_at_capacity_returns_zero() {
  futures::executor::block_on(async {
    let mut p: AsyncPeekable<_, Vec<u8>> =
      AsyncPeekable::with_capacity_and_buffer(Cursor::new(b"abcdefgh".to_vec()), 4);
    p.peek(&mut [0u8; 4]).await.unwrap();
    let n = p.fill_peek_buf().await.unwrap();
    assert_eq!(n, 0);
  });
}

// ------------------------------------------------------------------
// AsyncWrite delegation.
// ------------------------------------------------------------------

#[test]
fn async_write_delegates() {
  use futures::io::AsyncWriteExt;
  futures::executor::block_on(async {
    let r = FlakyReader::new(b"".to_vec(), vec![]);
    let mut p = AsyncPeekable::new(r);
    p.write_all(b"hi").await.unwrap();
    p.flush().await.unwrap();
    p.close().await.unwrap();
  });
}

// ------------------------------------------------------------------
// Error paths in helper futures.
// ------------------------------------------------------------------

#[test]
fn peek_to_end_propagates_error() {
  futures::executor::block_on(async {
    let r = FlakyReader::new(
      b"abc".to_vec(),
      vec![Action::ReadFromInner, Action::OtherErr],
    );
    let mut p = AsyncPeekable::new(r);
    let mut out = Vec::new();
    let err = p.peek_to_end(&mut out).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
  });
}

#[test]
fn peek_to_string_propagates_error() {
  futures::executor::block_on(async {
    let r = FlakyReader::new(
      b"hello".to_vec(),
      vec![Action::ReadFromInner, Action::OtherErr],
    );
    let mut p = AsyncPeekable::new(r);
    let mut s = String::new();
    let err = p.peek_to_string(&mut s).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
  });
}

#[test]
fn poll_read_no_buffer_inner_error() {
  futures::executor::block_on(async {
    let r = FlakyReader::new(b"".to_vec(), vec![Action::OtherErr]);
    let mut p = AsyncPeekable::new(r);
    let mut out = [0u8; 4];
    let err = p.read(&mut out).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
  });
}

#[test]
fn poll_peek_no_buffer_inner_error() {
  futures::executor::block_on(async {
    let r = FlakyReader::new(b"".to_vec(), vec![Action::OtherErr]);
    let mut p = AsyncPeekable::new(r);
    let mut out = [0u8; 4];
    let err = p.peek(&mut out).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
  });
}

#[test]
fn read_when_request_greater_than_buffer_error() {
  futures::executor::block_on(async {
    let r = FlakyReader::new(
      b"hello".to_vec(),
      vec![Action::ReadFromInner, Action::OtherErr],
    );
    let mut p = AsyncPeekable::new(r);
    p.peek(&mut [0u8; 2]).await.unwrap();
    let mut out = [0u8; 5];
    let err = p.read(&mut out).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
  });
}

// ------------------------------------------------------------------
// Trait deref impls (Box<dyn>, &mut, Pin<P>).
// ------------------------------------------------------------------

#[test]
fn async_peek_through_box_dyn() {
  futures::executor::block_on(async {
    let p = AsyncPeekable::new(Cursor::new(b"abc".to_vec()));
    let mut boxed: Box<dyn AsyncPeek + Unpin> = Box::new(p);
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut out = [0u8; 3];
    match Pin::new(&mut boxed).poll_peek(&mut cx, &mut out) {
      Poll::Ready(Ok(n)) => assert_eq!(n, 3),
      other => panic!("unexpected: {:?}", other),
    }
    assert_eq!(&out, b"abc");
  });
}

#[test]
fn async_peek_through_mut_ref() {
  futures::executor::block_on(async {
    let mut p = AsyncPeekable::new(Cursor::new(b"abc".to_vec()));
    let mut peek_ref: &mut (dyn AsyncPeek + Unpin) = &mut p;
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut out = [0u8; 3];
    match Pin::new(&mut peek_ref).poll_peek(&mut cx, &mut out) {
      Poll::Ready(Ok(_)) => {}
      other => panic!("unexpected: {:?}", other),
    }
  });
}

#[test]
fn async_peek_through_box_dyn_vectored() {
  futures::executor::block_on(async {
    let p = AsyncPeekable::new(Cursor::new(b"abc".to_vec()));
    let mut boxed: Box<dyn AsyncPeek + Unpin> = Box::new(p);
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut a = [0u8; 3];
    let mut bufs = [IoSliceMut::new(&mut a)];
    match Pin::new(&mut boxed).poll_peek_vectored(&mut cx, &mut bufs) {
      Poll::Ready(Ok(n)) => assert_eq!(n, 3),
      other => panic!("unexpected: {:?}", other),
    }
  });
}

// ------------------------------------------------------------------
// Manual-poll tests for Pending branches (need explicit polling
// because `await` would re-drive the future to Ready).
// ------------------------------------------------------------------

#[test]
fn fill_peek_buf_pending_truncates_buffer() {
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
  // length 0 — no ghost zero-bytes from the resize.
  assert_eq!(peekable::buffer::Buffer::len(&p.consume()), 0);
}

#[test]
fn poll_peek_no_buffer_pending() {
  // Cover the `else` (no buffer) Pending arm in poll_peek.
  use std::future::Future;

  let r = FlakyReader::new(b"".to_vec(), vec![Action::Pending]);
  let mut p = AsyncPeekable::new(r);
  let mut out = [0u8; 4];

  let mut fut = Box::pin(p.peek(&mut out));
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  match fut.as_mut().poll(&mut cx) {
    Poll::Pending => {} // expected
    other => panic!("unexpected: {:?}", other),
  }
}

#[test]
fn async_peek_through_pin_box() {
  // Cover `impl<P> AsyncPeek for Pin<P>` (poll_peek and
  // poll_peek_vectored).
  let p = AsyncPeekable::new(Cursor::new(b"abc".to_vec()));
  let mut pinned: Pin<Box<AsyncPeekable<Cursor<Vec<u8>>>>> = Box::pin(p);

  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 3];
  match Pin::new(&mut pinned).poll_peek(&mut cx, &mut out) {
    Poll::Ready(Ok(_)) => {}
    other => panic!("unexpected: {:?}", other),
  }

  let mut a = [0u8; 3];
  let mut bufs = [IoSliceMut::new(&mut a)];
  match Pin::new(&mut pinned).poll_peek_vectored(&mut cx, &mut bufs) {
    Poll::Ready(Ok(_)) => {}
    other => panic!("unexpected: {:?}", other),
  }
}

#[test]
fn poll_read_greater_branch_pending_partial_returns_buffered_data() {
  // Cover futures::AsyncRead Greater-Pending branch.
  use futures_util::AsyncRead as FutAsyncRead;
  use std::future::Future;

  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![Action::ReadFromInner, Action::Pending],
  );
  let mut p = AsyncPeekable::new(r);
  futures::executor::block_on(async {
    p.peek(&mut [0u8; 3]).await.unwrap();
  });

  // Now poll_read with a larger buf — the Pending branch must clear
  // our internal buffer and return Ready(Ok(buffer_len)).
  let waker = futures::task::noop_waker();
  let mut cx = Context::from_waker(&waker);
  let mut out = [0u8; 5];
  let pinned = Pin::new(&mut p);
  let res = FutAsyncRead::poll_read(pinned, &mut cx, &mut out);
  match res {
    Poll::Ready(Ok(n)) => assert_eq!(n, 3),
    other => panic!("unexpected: {:?}", other),
  }
  assert_eq!(&out[..3], b"hel");
}

// ------------------------------------------------------------------
// Regression: peek_to_end must not duplicate the prefix on re-poll
// after Pending. Uses FlakyReader to force a Pending mid-stream.
// ------------------------------------------------------------------

#[test]
fn peek_to_end_survives_pending_boundary() {
  futures::executor::block_on(async {
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
  });
}

// ------------------------------------------------------------------
// Regression: peek_to_string with a peek buffer ending mid-codepoint
// must not spuriously reject the stream as invalid UTF-8.
// ------------------------------------------------------------------

#[test]
fn peek_to_string_with_mid_codepoint_peek_buffer() {
  futures::executor::block_on(async {
    // "é" is U+00E9, encoded as [0xC3, 0xA9] in UTF-8.
    // We'll arrange the peek buffer to end after the first byte
    // (0xC3) — an incomplete sequence — and let peek_to_string
    // read the second byte (0xA9) from the inner reader.
    let data = "héllo".as_bytes().to_vec();
    let r = FlakyReader::new(data, vec![Action::ReadFromInner]);
    let mut p = AsyncPeekable::new(r);

    // Peek exactly 2 bytes: 'h' (0x68) + first byte of 'é' (0xC3).
    // The peek buffer now ends with an incomplete UTF-8 sequence.
    let mut pre = [0u8; 2];
    p.peek_exact(&mut pre).await.unwrap();
    assert_eq!(&pre, &[0x68, 0xC3]);

    // peek_to_string must NOT reject this as InvalidData — it
    // should read the remaining bytes and produce valid UTF-8.
    let mut s = String::new();
    let n = p.peek_to_string(&mut s).await.unwrap();
    assert_eq!(s, "héllo");
    assert_eq!(n, "héllo".len());
  });
}

#[test]
fn peek_to_end_keeps_partial_data_on_error() {
  struct ReadThenError {
    data: &'static [u8],
    emitted: bool,
  }

  impl AsyncRead for ReadThenError {
    fn poll_read(
      mut self: Pin<&mut Self>,
      _cx: &mut Context<'_>,
      buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
      if !self.emitted {
        self.emitted = true;
        let n = self.data.len().min(buf.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        Poll::Ready(Ok(n))
      } else {
        Poll::Ready(Err(io::Error::new(ErrorKind::Other, "boom")))
      }
    }
  }

  futures::executor::block_on(async {
    let r = ReadThenError {
      data: b"abc",
      emitted: false,
    };
    let mut p = AsyncPeekable::new(r);

    let mut out = b"sentinel".to_vec();

    let err = p.peek_to_end(&mut out).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
    // Partial data stays in buf — matching std's read_to_end contract.
    // "abc" was read before the error, so buf has "sentinel" + "abc".
    assert!(out.starts_with(b"sentinel"));
    assert!(out.len() > 8);
  });
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
      return Err(io::Error::new(io::ErrorKind::OutOfMemory, "BoundedBuffer cap"));
    }
    self.inner.resize(len, 0);
    Ok(())
  }
  fn truncate(&mut self, len: usize) {
    self.inner.truncate(len);
  }
  fn extend_from_slice(&mut self, other: &[u8]) -> io::Result<()> {
    if self.inner.len() + other.len() > CAP {
      return Err(io::Error::new(io::ErrorKind::OutOfMemory, "BoundedBuffer cap"));
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
    cx: &mut Context<'_>,
    buf: &mut [u8],
  ) -> Poll<io::Result<usize>> {
    let me = self.get_mut();
    if me.served >= me.error_after {
      return Poll::Ready(Err(io::Error::new(me.kind, "synthetic")));
    }
    let cap = (me.error_after - me.served).min(buf.len());
    match Pin::new(&mut me.data).poll_read(cx, &mut buf[..cap]) {
      Poll::Ready(Ok(n)) => {
        me.served += n;
        Poll::Ready(Ok(n))
      }
      other => other,
    }
  }
}

#[test]
fn async_peek_does_not_advance_reader_when_buffer_extend_fails() {
  futures::executor::block_on(async {
    let reader = Cursor::new(vec![1u8, 2, 3, 4]);
    let mut p: AsyncPeekable<_, BoundedBuffer<0>> = reader.peekable_with_buffer();
    let mut buf = [0u8; 2];
    let err = p.peek(&mut buf).await.expect_err("must fail");
    assert_eq!(err.kind(), ErrorKind::OutOfMemory);
    let (_, reader) = p.get_mut();
    let mut out = Vec::new();
    reader.read_to_end(&mut out).await.unwrap();
    assert_eq!(out, [1, 2, 3, 4]);
  });
}

#[test]
fn async_peek_exact_does_not_advance_reader_when_buffer_extend_fails() {
  futures::executor::block_on(async {
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
  });
}

#[test]
fn async_peek_to_end_preserves_partial_data_on_io_error() {
  futures::executor::block_on(async {
    let reader = AsyncErroringReader::new(b"hello".to_vec(), 3, ErrorKind::ConnectionReset);
    let mut p = reader.peekable();
    let mut out = Vec::new();
    let err = p.peek_to_end(&mut out).await.expect_err("io error");
    assert_eq!(err.kind(), ErrorKind::ConnectionReset);
    assert_eq!(out, b"hel");
  });
}

#[test]
fn async_peek_to_end_does_not_desync_on_buffer_error() {
  futures::executor::block_on(async {
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
  });
}

#[test]
fn async_peek_to_string_preserves_partial_valid_utf8_on_io_error() {
  futures::executor::block_on(async {
    let reader = AsyncErroringReader::new(b"hello".to_vec(), 3, ErrorKind::ConnectionReset);
    let mut p = reader.peekable();
    let mut out = String::new();
    let err = p.peek_to_string(&mut out).await.expect_err("io error");
    assert_eq!(err.kind(), ErrorKind::ConnectionReset);
    assert_eq!(out, "hel");
  });
}

#[test]
fn async_peek_to_string_leaves_buf_unchanged_when_partial_bytes_are_invalid_utf8() {
  futures::executor::block_on(async {
    let reader = AsyncErroringReader::new(vec![0xE2, 0x82, 0xAC], 2, ErrorKind::ConnectionReset);
    let mut p = reader.peekable();
    let mut out = String::from("prefix:");
    let err = p.peek_to_string(&mut out).await.expect_err("io error");
    assert_eq!(err.kind(), ErrorKind::ConnectionReset);
    assert_eq!(out, "prefix:");
  });
}

#[test]
fn async_peek_to_string_returns_invalid_data_for_clean_invalid_utf8() {
  futures::executor::block_on(async {
    let reader = Cursor::new(vec![0xFF, 0xFE]);
    let mut p = reader.peekable();
    let mut out = String::from("prefix:");
    let err = p.peek_to_string(&mut out).await.expect_err("must fail");
    assert_eq!(err.kind(), ErrorKind::InvalidData);
    assert_eq!(out, "prefix:");
  });
}
