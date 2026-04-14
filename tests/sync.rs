//! Integration tests for the synchronous `Peekable`.
//!
//! Includes regression tests for bugs found in the deep review:
//! - Bug 4: `Interrupted` is retried per the documented contract.
//! - Bug 5: read errors do not leave the peek buffer holding ghost
//!   zero-bytes from a transient `resize`.

use std::{
  io::{self, Cursor, ErrorKind, Read},
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
};

use peekable::PeekExt;

// ------------------------------------------------------------------
// FlakyReader: deterministically returns Interrupted/WouldBlock/Other
// errors on configured iterations. Used to exercise paths that a plain
// `Cursor` can never reach.
// ------------------------------------------------------------------

struct FlakyReader {
  inner: Cursor<Vec<u8>>,
  errors: Vec<Option<io::Error>>,
  call: Arc<AtomicUsize>,
}

impl FlakyReader {
  fn new(data: Vec<u8>, errors: Vec<Option<io::Error>>) -> Self {
    Self {
      inner: Cursor::new(data),
      errors,
      call: Arc::new(AtomicUsize::new(0)),
    }
  }

  fn calls(&self) -> Arc<AtomicUsize> {
    self.call.clone()
  }
}

impl Read for FlakyReader {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    let i = self.call.fetch_add(1, Ordering::SeqCst);
    if let Some(Some(err)) = self.errors.get(i) {
      return Err(io::Error::new(err.kind(), err.to_string()));
    }
    self.inner.read(buf)
  }
}

// ------------------------------------------------------------------
// Bug 4 regression: peek() retries on ErrorKind::Interrupted.
// ------------------------------------------------------------------

#[test]
fn peek_retries_on_interrupted_no_buffer() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      Some(io::Error::new(ErrorKind::Interrupted, "x")),
      Some(io::Error::new(ErrorKind::Interrupted, "y")),
      None,
    ],
  );
  let calls = r.calls();
  let mut p = r.peekable();
  let mut buf = [0u8; 5];
  let n = p.peek(&mut buf).unwrap();
  assert_eq!(n, 5);
  assert_eq!(&buf, b"hello");
  // 2 interrupted + 1 successful read.
  assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn peek_retries_on_interrupted_topup() {
  // Pre-fill the peek buffer with 2 bytes, then peek 5 — needs to read
  // more, which hits Interrupted before succeeding.
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      None,
      Some(io::Error::new(ErrorKind::Interrupted, "x")),
      None,
    ],
  );
  let mut p = r.peekable();
  let mut buf = [0u8; 2];
  p.peek(&mut buf).unwrap();
  assert_eq!(&buf, b"he");

  let mut buf = [0u8; 5];
  let n = p.peek(&mut buf).unwrap();
  assert_eq!(n, 5);
  assert_eq!(&buf, b"hello");
}

#[test]
fn peek_exact_retries_on_interrupted() {
  let r = FlakyReader::new(
    b"abcd".to_vec(),
    vec![Some(io::Error::new(ErrorKind::Interrupted, "x")), None],
  );
  let mut p = r.peekable();
  let mut buf = [0u8; 4];
  p.peek_exact(&mut buf).unwrap();
  assert_eq!(&buf, b"abcd");
}

#[test]
fn fill_peek_buf_retries_on_interrupted() {
  let r = FlakyReader::new(
    b"abcd".to_vec(),
    vec![Some(io::Error::new(ErrorKind::Interrupted, "x")), None],
  );
  let mut p = r.peekable_with_capacity(8);
  let n = p.fill_peek_buf().unwrap();
  assert_eq!(n, 4);
}

// ------------------------------------------------------------------
// Bug 5 regression: a read error during peek does NOT leave the peek
// buffer at the temporarily-resized length with ghost zero-bytes.
// ------------------------------------------------------------------

#[test]
fn peek_error_does_not_pollute_buffer_with_zero_bytes() {
  // First call succeeds (reads 2 bytes). Second peek wants 5 — needs
  // to read 3 more, but that read errors. Buffer must NOT keep the
  // zeroed-out positions.
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![None, Some(io::Error::new(ErrorKind::Other, "boom"))],
  );
  let mut p = r.peekable();
  let mut buf = [0u8; 2];
  p.peek(&mut buf).unwrap();
  assert_eq!(&buf, b"he");

  let mut buf = [0u8; 5];
  let err = p.peek(&mut buf).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);

  // Buffer should still claim 2 bytes (the originally-peeked "he"),
  // not 5 zero-bytes pretending to be data.
  let mut buf = [0u8; 2];
  let n = p.peek(&mut buf).unwrap();
  assert_eq!(n, 2);
  assert_eq!(&buf, b"he");
}

#[test]
fn fill_peek_buf_error_does_not_pollute_buffer() {
  let r = FlakyReader::new(
    b"abcd".to_vec(),
    vec![Some(io::Error::new(ErrorKind::Other, "boom"))],
  );
  let mut p = r.peekable_with_capacity(8);
  let err = p.fill_peek_buf().unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);

  // After the failed fill, the buffer should still be empty — no
  // ghost zero-bytes from the resize.
  let mut buf = [0u8; 4];
  // Subsequent peek should read the actual data, not zeros.
  let n = p.peek(&mut buf).unwrap();
  assert_eq!(n, 4);
  assert_eq!(&buf, b"abcd");
}

// ------------------------------------------------------------------
// Coverage for previously-untested code paths.
// ------------------------------------------------------------------

#[test]
fn peek_into_zero_length_buffer() {
  let mut p = Cursor::new(b"abc".to_vec()).peekable();
  let mut buf = [];
  assert_eq!(p.peek(&mut buf).unwrap(), 0);
}

#[test]
fn peek_to_end_then_read_to_end() {
  let mut p = Cursor::new(b"hello world".to_vec()).peekable();
  let mut out = Vec::new();
  let n = p.peek_to_end(&mut out).unwrap();
  assert_eq!(n, 11);
  assert_eq!(out, b"hello world");

  let mut out = Vec::new();
  let n = p.read_to_end(&mut out).unwrap();
  assert_eq!(n, 11);
  assert_eq!(out, b"hello world");
}

#[test]
fn peek_to_string_invalid_utf8() {
  let mut p = Cursor::new(vec![0xFF, 0xFF]).peekable();
  let mut s = String::new();
  let err = p.peek_to_string(&mut s).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::InvalidData);
}

#[test]
fn peek_to_string_invalid_utf8_in_buffer() {
  // Pre-fill peek buffer with valid UTF-8, then call peek_to_string —
  // exercises the early-validation path.
  let mut p = Cursor::new(b"abc\xff".to_vec()).peekable();
  let mut buf = [0u8; 4];
  p.peek(&mut buf).unwrap(); // buffer now contains 4 bytes including invalid UTF-8
  let mut s = String::new();
  let err = p.peek_to_string(&mut s).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::InvalidData);
}

#[test]
fn peek_exact_eof() {
  let mut p = Cursor::new(b"ab".to_vec()).peekable();
  let mut buf = [0u8; 5];
  let err = p.peek_exact(&mut buf).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
}

#[test]
fn peek_exact_smaller_than_buffer_returns_immediately() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  p.peek(&mut [0u8; 4]).unwrap(); // fill peek buffer with 4 bytes
  let mut buf = [0u8; 2];
  p.peek_exact(&mut buf).unwrap(); // less than peek buffer
  assert_eq!(&buf, b"ab");
}

#[test]
fn peek_equal_to_buffer_size() {
  let mut p = Cursor::new(b"abc".to_vec()).peekable();
  p.peek(&mut [0u8; 3]).unwrap(); // peek buffer has 3 bytes
  let mut buf = [0u8; 3];
  let n = p.peek(&mut buf).unwrap();
  assert_eq!(n, 3);
  assert_eq!(&buf, b"abc");
}

#[test]
fn fill_peek_buf_when_already_full_returns_zero() {
  // Use Vec backend so capacity() returns exactly the requested cap.
  // SmallVec's 64-byte inline capacity would mask this.
  let mut p: peekable::Peekable<_, Vec<u8>> =
    peekable::Peekable::with_capacity_and_buffer(Cursor::new(b"abcde".to_vec()), 3);
  let n = p.peek(&mut [0u8; 3]).unwrap();
  assert_eq!(n, 3); // fill buffer to its configured capacity

  let n = p.fill_peek_buf().unwrap();
  assert_eq!(n, 0);

  let mut buf = [0u8; 3];
  let n = p.peek(&mut buf).unwrap();
  assert_eq!(n, 3);
  assert_eq!(&buf, b"abc");
}

#[test]
fn read_after_peek_partial() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  p.peek(&mut [0u8; 4]).unwrap();

  // Read only 2 — should consume from peek buffer.
  let mut buf = [0u8; 2];
  let n = p.read(&mut buf).unwrap();
  assert_eq!(n, 2);
  assert_eq!(&buf, b"ab");

  // Read 4 more — partial from buffer (cd) + read from inner (ef).
  let mut buf = [0u8; 4];
  let n = p.read(&mut buf).unwrap();
  assert_eq!(n, 4);
  assert_eq!(&buf, b"cdef");
}

// ------------------------------------------------------------------
// Buffer trait coverage: SmallVec (default), Vec, TinyVec.
// ------------------------------------------------------------------

#[test]
fn peek_with_vec_buffer() {
  let mut p: peekable::Peekable<_, Vec<u8>> = Cursor::new(b"abc".to_vec()).peekable_with_buffer();
  let mut buf = [0u8; 3];
  p.peek(&mut buf).unwrap();
  assert_eq!(&buf, b"abc");
}

#[cfg(feature = "tinyvec")]
#[test]
fn peek_with_tinyvec_buffer() {
  let mut p: peekable::Peekable<_, tinyvec::TinyVec<[u8; 16]>> =
    Cursor::new(b"abc".to_vec()).peekable_with_buffer();
  let mut buf = [0u8; 3];
  p.peek(&mut buf).unwrap();
  assert_eq!(&buf, b"abc");
}

#[test]
fn peek_with_capacity_and_buffer_vec() {
  let mut p: peekable::Peekable<_, Vec<u8>> =
    Cursor::new(b"abc".to_vec()).peekable_with_capacity_and_buffer(64);
  let mut buf = [0u8; 3];
  p.peek(&mut buf).unwrap();
  assert_eq!(&buf, b"abc");
}

// ------------------------------------------------------------------
// API surface: From, get_mut, get_ref, into_components, consume.
// ------------------------------------------------------------------

#[test]
fn from_reader_and_from_tuple() {
  let p1 = peekable::Peekable::from(Cursor::new(b"a".to_vec()));
  let _ = p1.into_components();

  let p2 = peekable::Peekable::<Cursor<Vec<u8>>>::from((16usize, Cursor::new(b"a".to_vec())));
  let _ = p2.into_components();
}

#[test]
fn get_ref_and_get_mut() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  p.peek(&mut [0u8; 2]).unwrap();
  let (peeked, _r) = p.get_ref();
  assert_eq!(peeked, b"ab");
  let (peeked, _r) = p.get_mut();
  assert_eq!(peeked, b"ab");
}

#[test]
fn consume_and_consume_in_place() {
  let mut p = Cursor::new(b"abcdefgh".to_vec()).peekable();
  p.peek(&mut [0u8; 2]).unwrap();
  let buf = p.consume();
  assert_eq!(peekable::buffer::Buffer::as_slice(&buf), b"ab");

  p.peek(&mut [0u8; 2]).unwrap();
  p.consume_in_place();
  let mut out = [0u8; 2];
  let n = p.peek(&mut out).unwrap();
  assert_eq!(n, 2);
}

#[test]
fn consume_with_capacity_uses_capacity() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable_with_capacity(64);
  p.peek(&mut [0u8; 2]).unwrap();
  let buf = p.consume();
  assert!(peekable::buffer::Buffer::capacity(&buf) >= 64);
}

#[test]
fn into_components_returns_buffer_and_reader() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  p.peek(&mut [0u8; 2]).unwrap();
  let (buf, _r) = p.into_components();
  assert_eq!(peekable::buffer::Buffer::as_slice(&buf), b"ab");
}

// ------------------------------------------------------------------
// Read trait coverage: Less, Equal, Greater branches when peek buffer
// has data, plus Greater error path.
// ------------------------------------------------------------------

#[test]
fn read_when_request_less_than_buffer() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  p.peek(&mut [0u8; 4]).unwrap();
  let mut out = [0u8; 2];
  let n = p.read(&mut out).unwrap();
  assert_eq!(n, 2);
  assert_eq!(&out, b"ab");
}

#[test]
fn read_when_request_equal_to_buffer() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  p.peek(&mut [0u8; 4]).unwrap();
  let mut out = [0u8; 4];
  let n = p.read(&mut out).unwrap();
  assert_eq!(n, 4);
  assert_eq!(&out, b"abcd");
}

#[test]
fn read_when_request_greater_than_buffer_error() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![None, Some(io::Error::new(ErrorKind::Other, "boom"))],
  );
  let mut p = r.peekable();
  p.peek(&mut [0u8; 2]).unwrap();
  let mut out = [0u8; 5];
  let err = p.read(&mut out).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

// ------------------------------------------------------------------
// Write trait delegation.
// ------------------------------------------------------------------

#[test]
fn write_delegates_to_inner() {
  use std::io::Write;
  let mut p = peekable::Peekable::new(Vec::<u8>::new());
  p.write_all(b"hello").unwrap();
  p.flush().unwrap();
  let (_buf, inner) = p.into_components();
  assert_eq!(inner, b"hello");
}

// ------------------------------------------------------------------
// peek_vectored coverage.
// ------------------------------------------------------------------

#[test]
fn peek_vectored_basic() {
  use std::io::IoSliceMut;
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  let mut a = [0u8; 3];
  let mut bufs = [IoSliceMut::new(&mut a)];
  let n = p.peek_vectored(&mut bufs).unwrap();
  assert_eq!(n, 3);
  assert_eq!(&a, b"abc");
}

#[test]
fn peek_vectored_skips_empty_buffer() {
  use std::io::IoSliceMut;
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  let mut a: [u8; 0] = [];
  let mut b = [0u8; 3];
  let mut bufs = [IoSliceMut::new(&mut a), IoSliceMut::new(&mut b)];
  let n = p.peek_vectored(&mut bufs).unwrap();
  assert_eq!(n, 3);
  assert_eq!(&b, b"abc");
}

#[test]
fn peek_vectored_all_empty() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  let mut bufs: [io::IoSliceMut<'_>; 0] = [];
  let n = p.peek_vectored(&mut bufs).unwrap();
  assert_eq!(n, 0);
}

// ------------------------------------------------------------------
// Buffer trait method coverage (call methods directly on each impl).
// ------------------------------------------------------------------

// Helper exercises every Buffer trait method through fully-qualified
// trait syntax so we don't collide with the same-named inherent
// methods on Vec/SmallVec/TinyVec (which have different signatures).
fn exercise_buffer_trait<B: peekable::buffer::Buffer>() {
  use peekable::buffer::Buffer;
  let mut b = <B as Buffer>::new();
  assert!(<B as Buffer>::is_empty(&b));
  assert_eq!(<B as Buffer>::len(&b), 0);
  <B as Buffer>::extend_from_slice(&mut b, b"hello").unwrap();
  assert!(!<B as Buffer>::is_empty(&b));
  assert_eq!(<B as Buffer>::len(&b), 5);
  assert_eq!(<B as Buffer>::as_slice(&b), b"hello");
  let _ = <B as Buffer>::as_mut_slice(&mut b);
  <B as Buffer>::resize(&mut b, 10).unwrap();
  assert_eq!(<B as Buffer>::len(&b), 10);
  <B as Buffer>::truncate(&mut b, 5);
  assert_eq!(<B as Buffer>::len(&b), 5);
  <B as Buffer>::consume(&mut b, ..2);
  assert_eq!(<B as Buffer>::as_slice(&b), b"llo");
  let _ = <B as Buffer>::capacity(&b);
  <B as Buffer>::clear(&mut b);
  assert!(<B as Buffer>::is_empty(&b));

  let b2 = <B as Buffer>::with_capacity(64);
  assert!(<B as Buffer>::capacity(&b2) >= 64);
}

#[test]
fn buffer_trait_methods_vec() {
  exercise_buffer_trait::<Vec<u8>>();
}

#[cfg(feature = "smallvec")]
#[test]
fn buffer_trait_methods_smallvec() {
  exercise_buffer_trait::<smallvec::SmallVec<[u8; 8]>>();
}

#[cfg(feature = "tinyvec")]
#[test]
fn buffer_trait_methods_tinyvec() {
  exercise_buffer_trait::<tinyvec::TinyVec<[u8; 8]>>();
}

// ------------------------------------------------------------------
// More peek branches: Less and Equal via direct peek.
// ------------------------------------------------------------------

#[test]
fn peek_less_than_buffer_returns_partial() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  p.peek(&mut [0u8; 4]).unwrap(); // buffer = "abcd"
  let mut out = [0u8; 2];
  let n = p.peek(&mut out).unwrap();
  assert_eq!(n, 2);
  assert_eq!(&out, b"ab");
}

#[test]
fn peek_equal_to_buffer_returns_all() {
  let mut p = Cursor::new(b"abcd".to_vec()).peekable();
  p.peek(&mut [0u8; 4]).unwrap();
  let mut out = [0u8; 4];
  let n = p.peek(&mut out).unwrap();
  assert_eq!(n, 4);
  assert_eq!(&out, b"abcd");
}

// ------------------------------------------------------------------
// peek_exact when want > peek buffer (no Interrupted, just success path).
// ------------------------------------------------------------------

#[test]
fn peek_exact_topup_from_inner() {
  let mut p = Cursor::new(b"abcdef".to_vec()).peekable();
  p.peek(&mut [0u8; 2]).unwrap(); // buffer has 2 bytes
  let mut buf = [0u8; 5];
  p.peek_exact(&mut buf).unwrap(); // needs to top up by 3
  assert_eq!(&buf, b"abcde");
}

#[test]
fn peek_exact_error_during_read() {
  let r = FlakyReader::new(
    b"abc".to_vec(),
    vec![Some(io::Error::new(ErrorKind::Other, "boom"))],
  );
  let mut p = r.peekable();
  let mut buf = [0u8; 4];
  let err = p.peek_exact(&mut buf).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

#[test]
fn peek_to_end_propagates_error() {
  // read_to_end inside peek_to_end will hit the error and return it.
  let r = FlakyReader::new(
    b"abc".to_vec(),
    vec![
      None,                                           // first read returns "abc"
      Some(io::Error::new(ErrorKind::Other, "boom")), // next read errors
    ],
  );
  let mut p = r.peekable();
  let mut out = Vec::new();
  let err = p.peek_to_end(&mut out).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

#[test]
fn peek_to_string_propagates_error() {
  let r = FlakyReader::new(
    b"hello".to_vec(),
    vec![
      None,                                           // first read returns "hello"
      Some(io::Error::new(ErrorKind::Other, "boom")), // next read errors
    ],
  );
  let mut p = r.peekable();
  let mut s = String::new();
  let err = p.peek_to_string(&mut s).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}

#[test]
fn peek_propagates_non_interrupted_error_no_buffer() {
  // No buffered data, peek hits error directly. Covers the bottom
  // `Err(e) => return Err(e)` arm in peek's main loop.
  let r = FlakyReader::new(
    b"".to_vec(),
    vec![Some(io::Error::new(ErrorKind::Other, "boom"))],
  );
  let mut p = r.peekable();
  let mut out = [0u8; 4];
  let err = p.peek(&mut out).unwrap_err();
  assert_eq!(err.kind(), ErrorKind::Other);
}
