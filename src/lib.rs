//! Peakable peeker and async peeker
//!
//!
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

use buffer::Buffer;
use std::{
  cmp,
  io::{IoSliceMut, Read, Result, Write},
  mem,
};

#[doc(hidden)]
#[cfg(feature = "smallvec")]
pub type DefaultBuffer = smallvec::SmallVec<[u8; 64]>;

#[doc(hidden)]
#[cfg(not(feature = "smallvec"))]
pub type DefaultBuffer = Vec<u8>;

/// Extracts the successful type of a `Poll<T>`.
///
/// This macro bakes in propagation of `Pending` signals by returning early.
#[cfg(any(feature = "tokio", feature = "future"))]
#[macro_export]
macro_rules! ready {
  ($e:expr $(,)?) => {
    match $e {
      ::std::task::Poll::Ready(t) => t,
      ::std::task::Poll::Pending => return ::std::task::Poll::Pending,
    }
  };
}

/// Asynchronous peek I/O
///
/// This crate contains the `AsyncPeek` and `AsyncPeekExt`
/// traits, the asynchronous analogs to
/// `peekable::{Peek, Peekable}`. The primary difference is
/// that these traits integrate with the asynchronous task system.
///
/// All items of this library are only available when the `future` feature of this
/// library is activated, and it is not activated by default.
#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
pub mod future;

/// Traits, helpers, and type definitions for asynchronous peekable I/O functionality.
///
/// This module is the asynchronous version of `peekable::{Peek, Peekable}`. Primarily, it
/// defines one trait, [`AsyncPeek`], which is asynchronous
/// version of the [`Peek`] trait.
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub mod tokio;

/// The generic buffer trait used by the `Peekable` and `AsyncPeekable`.
pub mod buffer;

/// A wrapper around an [`Read`] types to make them support peek related methods.
pub struct Peekable<R, B = DefaultBuffer> {
  /// The inner reader.
  reader: R,
  /// The buffer used to store peeked bytes.
  buffer: B,
  buf_cap: Option<usize>,
}

impl<R, B> Read for Peekable<R, B>
where
  B: buffer::Buffer,
  R: Read,
{
  fn read(&mut self, mut buf: &mut [u8]) -> Result<usize> {
    let this = self;
    let want_peek = buf.len();
    let peek_buf = this.buffer.as_mut_slice();

    // check if the peek buffer has data
    let buffer_len = peek_buf.len();
    if buffer_len > 0 {
      return match want_peek.cmp(&buffer_len) {
        cmp::Ordering::Less => {
          buf.copy_from_slice(&peek_buf[..want_peek]);
          this.buffer.consume(..want_peek);
          return Ok(want_peek);
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(peek_buf);
          this.buffer.clear();
          return Ok(want_peek);
        }
        cmp::Ordering::Greater => {
          buf[..buffer_len].copy_from_slice(peek_buf);
          buf = &mut buf[buffer_len..];
          match this.reader.read(buf) {
            Ok(bytes) => {
              this.buffer.clear();
              Ok(bytes + buffer_len)
            }
            Err(e) => Err(e),
          }
        }
      };
    }

    this.reader.read(buf)
  }
}

impl<W, B> Write for Peekable<W, B>
where
  W: Write,
  B: Buffer,
{
  fn write(&mut self, buf: &[u8]) -> Result<usize> {
    self.reader.write(buf)
  }

  fn flush(&mut self) -> Result<()> {
    self.reader.flush()
  }
}

impl<R> From<R> for Peekable<R> {
  fn from(reader: R) -> Self {
    Peekable::new(reader)
  }
}

impl<R> From<(usize, R)> for Peekable<R> {
  fn from((cap, reader): (usize, R)) -> Self {
    Peekable::with_capacity(reader, cap)
  }
}

impl<R> Peekable<R> {
  /// Creates a new peekable wrapper around the given reader.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  ///
  /// let peekable = peekable::Peekable::new(Cursor::new([1, 2, 3, 4]));
  /// ```
  pub fn new(reader: R) -> Self {
    Self {
      reader,
      buffer: DefaultBuffer::new(),
      buf_cap: None,
    }
  }

  /// Creates a new peekable wrapper around the given reader with the specified
  /// capacity for the peek buffer.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  ///
  /// let peekable = peekable::Peekable::with_capacity(Cursor::new([0; 1024]), 1024);
  /// ```
  pub fn with_capacity(reader: R, capacity: usize) -> Self {
    Self {
      reader,
      buffer: DefaultBuffer::with_capacity(capacity),
      buf_cap: Some(capacity),
    }
  }
}

impl<R, B> Peekable<R, B> {
  /// Creates a new peekable wrapper around the given reader.
  /// This method allows you to specify a custom buffer type.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::{io::Cursor, vec::Vec};
  /// use peekable::Peekable;
  ///
  /// let peekable: Peekable<_, Vec<u8>> = Peekable::with_buffer(Cursor::new([1, 2, 3, 4]));
  /// ```
  pub fn with_buffer(reader: R) -> Self
  where
    B: Buffer,
  {
    Self {
      reader,
      buffer: B::new(),
      buf_cap: None,
    }
  }

  /// Creates a new peekable wrapper around the given reader with the specified
  /// capacity for the peek buffer, this method allows you to specify a custom buffer type.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::{io::Cursor, vec::Vec};
  /// use peekable::Peekable;
  ///
  /// let peekable: Peekable<_, Vec<u8>>  = Peekable::with_capacity_and_buffer(Cursor::new([0; 1024]), 1024);
  /// ```
  pub fn with_capacity_and_buffer(reader: R, capacity: usize) -> Self
  where
    B: Buffer,
  {
    Self {
      reader,
      buffer: B::with_capacity(capacity),
      buf_cap: Some(capacity),
    }
  }

  /// Clears the peek buffer.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  /// use peekable::Peekable;
  ///
  /// let mut peekable: Peekable<_> = Peekable::from(Cursor::new([1, 2, 3, 4]));
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let consumed = peekable.consume();
  /// assert_eq!(consumed.as_slice(), [1, 2].as_slice());
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [3, 4]);
  /// ```
  #[inline]
  pub fn consume(&mut self) -> B
  where
    B: Buffer,
  {
    let buf = match self.buf_cap {
      Some(capacity) => B::with_capacity(capacity),
      None => B::new(),
    };
    mem::replace(&mut self.buffer, buf)
  }

  /// Consumes the peek buffer in place so that the peek buffer can be reused and avoid allocating.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  /// use peekable::Peekable;
  ///
  /// let mut peekable: Peekable<_> = Peekable::from(Cursor::new([1, 2, 3, 4]));
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// peekable.consume_in_place();
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [3, 4]);
  /// ```
  #[inline]
  pub fn consume_in_place(&mut self)
  where
    B: Buffer,
  {
    self.buffer.clear();
  }

  /// Returns the bytes already be peeked into memory and a mutable reference to the underlying reader.
  ///
  /// **WARNING: If you invoke `AsyncRead` or `AsyncReadExt` methods on the underlying reader, may lead to unexpected read behaviors.**
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  ///
  /// let mut peekable = peekable::Peekable::new(Cursor::new([1, 2, 3, 4]));
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (peeked, reader) = peekable.get_mut();
  /// assert_eq!(peeked, [1, 2]);
  /// ```
  #[inline]
  pub fn get_mut(&mut self) -> (&[u8], &mut R)
  where
    B: AsRef<[u8]>,
  {
    (self.buffer.as_ref(), &mut self.reader)
  }

  /// Returns the bytes already be peeked into memory and a reference to the underlying reader.
  ///
  /// **WARNING: If you invoke `AsyncRead` or `AsyncReadExt` methods on the underlying reader, may lead to unexpected read behaviors.**
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  ///
  /// let mut peekable = peekable::Peekable::new(Cursor::new([1, 2, 3, 4]));
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (peeked, reader) = peekable.get_ref();
  /// assert_eq!(peeked, [1, 2]);
  /// ```
  #[inline]
  pub fn get_ref(&self) -> (&[u8], &R)
  where
    B: AsRef<[u8]>,
  {
    (self.buffer.as_ref(), &self.reader)
  }

  /// Consumes the `AsyncPeekable`, returning the a vec may contain the bytes already be peeked into memory and the wrapped reader.
  ///
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  ///
  /// let mut peekable = peekable::Peekable::new(Cursor::new([1, 2, 3, 4]));
  ///
  /// let mut output = [0u8; 2];
  ///
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (peeked, reader) = peekable.into_components();
  ///
  /// assert_eq!(peeked.as_slice(), [1, 2].as_slice());
  /// ```
  #[inline]
  pub fn into_components(self) -> (B, R) {
    (self.buffer, self.reader)
  }
}

impl<R, B> Peekable<R, B>
where
  B: Buffer,
  R: Read,
{
  /// Pull some bytes from this source into the specified buffer, returning
  /// how many bytes were peeked.
  ///
  /// This function does not provide any guarantees about whether it blocks
  /// waiting for data, but if an object needs to block for a peek and cannot,
  /// it will typically signal this via an [`Err`] return value.
  ///
  /// If the return value of this method is [`Ok(n)`], then implementations must
  /// guarantee that `0 <= n <= buf.len()`. A nonzero `n` value indicates
  /// that the buffer `buf` has been filled in with `n` bytes of data from this
  /// source. If `n` is `0`, then it can indicate one of two scenarios:
  ///
  /// 1. This peeker has reached its "end of file" and will likely no longer
  ///    be able to produce bytes. Note that this does not mean that the
  ///    peeker will *always* no longer be able to produce bytes. As an example,
  ///    on Linux, this method will call the `recv` syscall for a [`TcpStream`],
  ///    where returning zero indicates the connection was shut down correctly. While
  ///    for [`File`], it is possible to reach the end of file and get zero as result,
  ///    but if more data is appended to the file, future calls to `peek` will return
  ///    more data.
  /// 2. The buffer specified was 0 bytes in length.
  ///
  /// It is not an error if the returned value `n` is smaller than the buffer size,
  /// even when the peeker is not at the end of the stream yet.
  /// This may happen for example because fewer bytes are actually available right now
  /// (e. g. being close to end-of-file) or because peek() was interrupted by a signal.
  ///
  /// As this trait is safe to implement, callers in unsafe code cannot rely on
  /// `n <= buf.len()` for safety.
  /// Extra care needs to be taken when `unsafe` functions are used to access the peek bytes.
  /// Callers have to ensure that no unchecked out-of-bounds accesses are possible even if
  /// `n > buf.len()`.
  ///
  /// No guarantees are provided about the contents of `buf` when this
  /// function is called, so implementations cannot rely on any property of the
  /// contents of `buf` being true. It is recommended that *implementations*
  /// only write data to `buf` instead of peeking its contents.
  ///
  /// Correspondingly, however, *callers* of this method in unsafe code must not assume
  /// any guarantees about how the implementation uses `buf`. The trait is safe to implement,
  /// so it is possible that the code that's supposed to write to the buffer might also peek
  /// from it. It is your responsibility to make sure that `buf` is initialized
  /// before calling `peek`. Calling `peek` with an uninitialized `buf` (of the kind one
  /// obtains via [`MaybeUninit<T>`]) is not safe, and can lead to undefined behavior.
  ///
  /// [`MaybeUninit<T>`]: crate::mem::MaybeUninit
  ///
  /// # Errors
  ///
  /// If this function encounters any form of I/O or other error, an error
  /// variant will be returned. If an error is returned then it must be
  /// guaranteed that no bytes were peek.
  ///
  /// An error of the [`ErrorKind::Interrupted`] kind is non-fatal and the peek
  /// operation should be retried if there is nothing else to do.
  ///
  /// # Examples
  ///
  ///
  /// [`Ok(n)`]: Ok
  /// [`File`]: std::fs::File
  /// [`TcpStream`]: std::net::TcpStream
  ///
  /// ```rust
  /// use std::io;
  /// use std::io::{Cursor, Read};
  /// use peekable::PeekExt;
  ///
  /// # fn main() -> io::Result<()> {
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = [0u8; 5];
  ///
  /// let bytes = peekable.peek(&mut output[..3])?;
  ///
  /// // This is only guaranteed to be 4 because `&[u8]` is a synchronous
  /// // reader. In a real system you could get anywhere from 1 to
  /// // `output.len()` bytes in a single read.
  /// assert_eq!(bytes, 3);
  /// assert_eq!(output, [1, 2, 3, 0, 0]);
  ///
  /// // you can peek mutiple times
  ///
  /// let bytes = peekable.peek(&mut output[..])?;
  /// assert_eq!(bytes, 4);
  /// assert_eq!(output, [1, 2, 3, 4, 0]);
  ///
  /// // you can read after peek
  /// let mut output = [0u8; 5];
  /// let bytes = peekable.read(&mut output[..2])?;
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2, 0, 0, 0]);
  ///
  /// // peek after read
  /// let mut output = [0u8; 5];
  /// let bytes = peekable.peek(&mut output[..])?;
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [3, 4, 0, 0, 0]);
  /// # Ok(())
  /// # }
  /// ```
  pub fn peek(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    let want_peek = buf.len();

    // check if the peek buffer has data
    let buffer_len = self.buffer.len();

    if buffer_len > 0 {
      return match want_peek.cmp(&buffer_len) {
        cmp::Ordering::Less => {
          buf.copy_from_slice(&self.buffer.as_slice()[..want_peek]);
          Ok(want_peek)
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(self.buffer.as_slice());
          Ok(want_peek)
        }
        cmp::Ordering::Greater => {
          self.buffer.resize(want_peek)?;
          match self
            .reader
            .read(&mut self.buffer.as_mut_slice()[buffer_len..])
          {
            Ok(n) => {
              self.buffer.truncate(n + buffer_len);
              buf[..buffer_len + n].copy_from_slice(self.buffer.as_slice());
              Ok(buffer_len + n)
            }
            Err(e) => Err(e),
          }
        }
      };
    }

    let this = self;
    match this.reader.read(buf) {
      Ok(bytes) => {
        this.buffer.extend_from_slice(&buf[..bytes])?;
        Ok(bytes)
      }
      Err(e) => Err(e),
    }
  }

  /// Like `peek`, except that it peeks into a slice of buffers.
  ///
  /// Data is copied to fill each buffer in order, with the final buffer
  /// written to possibly being only partially filled. This method must
  /// behave equivalently to a single call to `peek` with concatenated
  /// buffers.
  ///
  /// The default implementation calls `peek` with either the first nonempty
  /// buffer provided, or an empty one if none exists.
  pub fn peek_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize> {
    for b in bufs {
      if !b.is_empty() {
        return self.peek(b);
      }
    }

    self.peek(&mut [])
  }

  /// Peek all bytes until EOF in this source, placing them into `buf`.
  ///
  /// All bytes peek from this source will be appended to the specified buffer
  /// `buf`. This function will continuously call [`peek()`] to append more data to
  /// `buf` until [`peek()`] returns either [`Ok(0)`] or an error of
  /// non-[`ErrorKind::Interrupted`] kind.
  ///
  /// If successful, this function will return the total number of bytes peek.
  ///
  /// # Errors
  ///
  /// If this function encounters an error of the kind
  /// [`ErrorKind::Interrupted`] then the error is ignored and the operation
  /// will continue.
  ///
  /// If any other peek error is encountered then this function immediately
  /// returns. Any bytes which have alpeeky been peek will be appended to
  /// `buf`.
  ///
  /// # Examples
  ///
  /// [`peek()`]: Peekable::peek
  /// [`Ok(0)`]: Ok
  ///
  /// ```rust
  /// use std::io;
  /// use std::io::{Cursor, Read};
  /// use peekable::PeekExt;
  ///
  /// # fn main() -> io::Result<()> {
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = Vec::with_capacity(4);
  ///
  /// let bytes = peekable.peek_to_end(&mut output)?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(output, vec![1, 2, 3, 4]);
  ///
  /// // read after peek
  /// let mut output = Vec::with_capacity(4);
  ///
  /// let bytes = peekable.read_to_end(&mut output)?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(output, vec![1, 2, 3, 4]);
  /// # Ok(())
  /// # }
  /// ```
  pub fn peek_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
    let this = &mut *self;
    let inbuf = this.buffer.len();

    let original_buf = buf.len();
    buf.extend_from_slice(this.buffer.as_slice());

    let fut = this.reader.read_to_end(buf);
    match fut {
      Ok(read) => {
        this
          .buffer
          .extend_from_slice(&buf[original_buf + inbuf..])?;
        Ok(read + inbuf)
      }
      Err(e) => Err(e),
    }
  }

  /// Peek all bytes until EOF in this source, appending them to `buf`.
  ///
  /// If successful, this function returns the number of bytes which were peek
  /// and appended to `buf`.
  ///
  /// # Errors
  ///
  /// If the data in this stream is *not* valid UTF-8 then an error is
  /// returned and `buf` is unchanged.
  ///
  /// See [`peek_to_end`] for other error semantics.
  ///
  /// [`peek_to_end`]: Peek::peek_to_end
  ///
  /// # Examples
  ///
  ///
  /// ```rust
  /// use std::io;
  /// use std::io::{Cursor, Read};
  /// use peekable::PeekExt;
  ///
  /// # fn main() -> io::Result<()> {
  ///
  /// let mut peekable = Cursor::new(&b"1234"[..]).peekable();
  /// let mut buffer = String::with_capacity(4);
  ///
  /// let bytes = peekable.peek_to_string(&mut buffer)?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(buffer, String::from("1234"));
  ///
  /// // read after peek
  /// let mut buffer = String::with_capacity(4);
  /// let bytes = peekable.peek_to_string(&mut buffer)?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(buffer, String::from("1234"));
  ///
  /// // peek invalid utf-8
  /// let mut peekable = Cursor::new([255; 4]).peekable();
  /// let mut buffer = String::with_capacity(4);
  /// assert!(peekable.peek_to_string(&mut buffer).is_err());
  /// # Ok(())
  /// # }
  /// ```
  pub fn peek_to_string(&mut self, buf: &mut String) -> Result<usize> {
    let s = match core::str::from_utf8(self.buffer.as_slice()) {
      Ok(s) => s,
      Err(_) => {
        return Err(std::io::Error::new(
          std::io::ErrorKind::InvalidData,
          "stream did not contain valid UTF-8",
        ))
      }
    };

    buf.push_str(s);

    let inbuf = self.buffer.len();
    let fut = self.reader.read_to_string(buf);
    match fut {
      Ok(read) => {
        self.buffer.extend_from_slice(&buf.as_bytes()[inbuf..])?;
        Ok(read + inbuf)
      }
      Err(e) => Err(e),
    }
  }

  /// Peek the exact number of bytes required to fill `buf`.
  ///
  /// This function peeks as many bytes as necessary to completely fill the
  /// specified buffer `buf`.
  ///
  /// No guarantees are provided about the contents of `buf` when this
  /// function is called, so implementations cannot rely on any property of the
  /// contents of `buf` being true. It is recommended that implementations
  /// only write data to `buf` instead of peeking its contents. The
  /// documentation on [`peek`] has a more detailed explanation on this
  /// subject.
  ///
  /// # Errors
  ///
  /// If this function encounters an error of the kind
  /// [`ErrorKind::Interrupted`](std::io::ErrorKind::Interrupted) then the error is ignored and the operation
  /// will continue.
  ///
  /// If this function encounters an "end of file" before completely filling
  /// the buffer, it returns an error of the kind [`ErrorKind::UnexpectedEof`](std::io::ErrorKind::Interrupted).
  /// The contents of `buf` are unspecified in this case.
  ///
  /// If any other peek error is encountered then this function immediately
  /// returns. The contents of `buf` are unspecified in this case.
  ///
  /// If this function returns an error, it is unspecified how many bytes it
  /// has peek, but it will never peek more than would be necessary to
  /// completely fill the buffer.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io;
  /// use std::io::{Cursor, Read};
  /// use peekable::PeekExt;
  ///
  /// # fn main() -> io::Result<()> {
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = [0u8; 4];
  ///
  /// peekable.peek_exact(&mut output)?;
  ///
  /// assert_eq!(output, [1, 2, 3, 4]);
  ///
  /// // read after peek
  /// let mut output = [0u8; 2];
  ///
  /// peekable.read_exact(&mut output[..])?;
  ///
  /// assert_eq!(output, [1, 2]);
  ///
  /// // peek after read
  /// let mut output = [0u8; 2];
  /// peekable.peek_exact(&mut output)?;
  ///
  /// assert_eq!(output, [3, 4]);
  /// # Ok(())
  /// # }
  /// ```
  ///
  /// ## EOF is hit before `buf` is filled
  ///
  /// ```
  /// use std::io;
  /// use std::io::{Cursor, Read};
  /// use peekable::PeekExt;
  ///
  /// # fn main() -> io::Result<()> {
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = [0u8; 5];
  ///
  /// let result = peekable.peek_exact(&mut output);
  /// assert_eq!(
  ///   result.unwrap_err().kind(),
  ///   std::io::ErrorKind::UnexpectedEof
  /// );
  ///
  /// let result = peekable.peek_exact(&mut output[..4]);
  /// assert!(result.is_ok());
  /// assert_eq!(output, [1, 2, 3, 4, 0]);
  ///
  /// # Ok(())
  /// # }
  /// ```
  pub fn peek_exact(&mut self, mut buf: &mut [u8]) -> Result<()> {
    let this = self;

    let buf_len = buf.len();
    let peek_buf_len = this.buffer.len();

    if buf_len <= peek_buf_len {
      buf.copy_from_slice(&this.buffer.as_slice()[..buf_len]);
      return Ok(());
    }

    buf[..peek_buf_len].copy_from_slice(this.buffer.as_slice());
    {
      let (_read, rest) = mem::take(&mut buf).split_at_mut(peek_buf_len);
      buf = rest;
    }
    let mut readed = peek_buf_len;
    while !buf.is_empty() {
      let n = this.reader.read(buf)?;
      {
        let (read, rest) = mem::take(&mut buf).split_at_mut(n);
        this.buffer.extend_from_slice(read)?;
        readed += n;
        buf = rest;
      }
      if n == 0 && readed != buf_len {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
      }
    }

    Ok(())
  }

  /// Try to fill the peek buffer with more data. Returns the number of bytes peeked.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io;
  /// use std::io::{Cursor, Read};
  /// use peekable::PeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable_with_capacity(5);
  /// let mut output = [0u8; 4];
  ///
  /// peekable.peek_exact(&mut output[..1]).unwrap();
  /// assert_eq!(output, [1, 0, 0, 0]);
  ///
  /// let bytes = peekable.fill_peek_buf().unwrap();
  /// assert_eq!(bytes, 3);
  ///
  /// let bytes = peekable.peek(&mut output).unwrap();
  /// assert_eq!(output, [1, 2, 3, 4].as_slice());
  /// ````
  pub fn fill_peek_buf(&mut self) -> Result<usize> {
    let cap = self.buffer.capacity();
    let cur = self.buffer.len();
    self.buffer.resize(cap)?;
    let peeked = self.reader.read(&mut self.buffer.as_mut_slice()[cur..])?;
    self.buffer.truncate(cur + peeked);
    Ok(peeked)
  }
}

/// An extension trait which adds utility methods to [`Read`] types.
pub trait PeekExt: Read {
  /// Wraps a [`Read`] type in a `Peekable` which provides a `peek` related methods.
  fn peekable(self) -> Peekable<Self>
  where
    Self: Sized,
  {
    Peekable::new(self)
  }

  /// Wraps a [`Read`] type in a `Peekable` which provides a `peek` related methods with a specified capacity.
  fn peekable_with_capacity(self, capacity: usize) -> Peekable<Self>
  where
    Self: Sized,
  {
    Peekable::with_capacity(self, capacity)
  }

  /// Creates a new `Peekable` which will wrap the given reader.
  ///
  /// This method allows you to specify a custom buffer type.
  fn peekable_with_buffer<B>(self) -> Peekable<Self, B>
  where
    Self: Sized,
    B: Buffer,
  {
    Peekable::with_buffer(self)
  }

  /// Wraps a [`Read`] type in a `Peekable` which provides a `peek` related methods with a specified capacity.
  ///
  /// This method allows you to specify a custom buffer type.
  fn peekable_with_capacity_and_buffer<B>(self, capacity: usize) -> Peekable<Self, B>
  where
    Self: Sized,
    B: Buffer,
  {
    Peekable::with_capacity_and_buffer(self, capacity)
  }
}

impl<R: Read + ?Sized> PeekExt for R {}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Cursor;

  #[test]
  fn test_peek_exact_peek_exact_read_exact() {
    let mut peekable = Cursor::new([1, 2, 3, 4, 5, 6, 7, 8, 9]).peekable();
    let mut buf1 = [0; 2];
    peekable.peek_exact(&mut buf1).unwrap();
    assert_eq!(buf1, [1, 2]);

    let mut buf2 = [0; 4];
    peekable.peek_exact(&mut buf2).unwrap();
    assert_eq!(buf2, [1, 2, 3, 4]);

    let mut buf3 = [0; 4];
    peekable.read_exact(&mut buf3).unwrap();
    assert_eq!(buf3, [1, 2, 3, 4]);
  }

  #[test]
  fn test_peek_exact_peek_exact_read_exact_2() {
    let mut peekable = Cursor::new([1, 2, 3, 4, 5, 6, 7, 8, 9]).peekable_with_buffer::<Vec<u8>>();
    let mut buf1 = [0; 2];
    peekable.peek_exact(&mut buf1).unwrap();
    assert_eq!(buf1, [1, 2]);

    let mut buf2 = [0; 4];
    peekable.peek_exact(&mut buf2).unwrap();
    assert_eq!(buf2, [1, 2, 3, 4]);

    let mut buf3 = [0; 4];
    peekable.read_exact(&mut buf3).unwrap();
    assert_eq!(buf3, [1, 2, 3, 4]);
  }

  #[test]
  fn test_peek_exact_peek_exact_read_exact_3() {
    let mut peekable =
      Cursor::new([1, 2, 3, 4, 5, 6, 7, 8, 9]).peekable_with_capacity_and_buffer::<Vec<u8>>(24);
    let mut buf1 = [0; 2];
    peekable.peek_exact(&mut buf1).unwrap();
    assert_eq!(buf1, [1, 2]);

    let mut buf2 = [0; 4];
    peekable.peek_exact(&mut buf2).unwrap();
    assert_eq!(buf2, [1, 2, 3, 4]);

    let mut buf3 = [0; 4];
    peekable.read_exact(&mut buf3).unwrap();
    assert_eq!(buf3, [1, 2, 3, 4]);
  }
}
