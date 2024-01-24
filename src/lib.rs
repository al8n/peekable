//! Peakable peeker and async peeker
//!
//!
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

use std::{
  cmp,
  io::{IoSliceMut, Read, Result},
};

#[cfg(feature = "smallvec")]
type Buffer = smallvec::SmallVec<[u8; 64]>;

#[cfg(not(feature = "smallvec"))]
type Buffer = Vec<u8>;

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

/// A wrapper around an [`Read`] types to make them support [`Peek`] methods.
pub struct Peekable<R> {
  /// The inner peeker.
  peeker: R,
  /// The buffer used to store peeked bytes.
  buffer: Buffer,
}

impl<R: Read> Read for Peekable<R> {
  fn read(&mut self, mut buf: &mut [u8]) -> Result<usize> {
    let this = self;
    let want_peek = buf.len();

    // check if the peek buffer has data
    let buffer_len = this.buffer.len();
    if buffer_len > 0 {
      return match want_peek.cmp(&buffer_len) {
        cmp::Ordering::Less => {
          buf.copy_from_slice(&this.buffer[..want_peek]);
          this.buffer.drain(..want_peek);
          return Ok(want_peek);
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(&this.buffer);
          this.buffer.clear();
          return Ok(want_peek);
        }
        cmp::Ordering::Greater => {
          buf[..buffer_len].copy_from_slice(&this.buffer);
          buf = &mut buf[buffer_len..];
          match this.peeker.read(buf) {
            Ok(bytes) => {
              this.buffer.clear();
              Ok(bytes + buffer_len)
            }
            Err(e) => Err(e),
          }
        }
      };
    }

    this.peeker.read(buf)
  }
}

impl<R> From<R> for Peekable<R> {
  fn from(peeker: R) -> Self {
    Peekable {
      peeker,
      buffer: Buffer::new(),
    }
  }
}

impl<R> Peekable<R> {
  /// Creates a new peekable wrapper around the given reader.
  pub fn new(reader: R) -> Self {
    Self {
      peeker: reader,
      buffer: Buffer::new(),
    }
  }

  /// Creates a new peekable wrapper around the given reader with the specified
  /// capacity for the peek buffer.
  pub fn with_capacity(reader: R, capacity: usize) -> Self {
    Self {
      peeker: reader,
      buffer: Buffer::with_capacity(capacity),
    }
  }
}

impl<R: Read> Peekable<R> {
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
  /// [`File`]s implement `Read`:
  ///
  /// [`Ok(n)`]: Ok
  /// [`File`]: std::fs::File
  /// [`TcpStream`]: std::net::TcpStream
  ///
  /// ```no_run
  /// use std::io;
  /// use std::io::prelude::*;
  /// use std::fs::File;
  /// use peekable::PeekExt;
  ///
  /// fn main() -> io::Result<()> {
  ///     let mut f = File::open("foo.txt")?.peekable();
  ///     let mut buffer = [0; 10];
  ///
  ///     // peek up to 10 bytes
  ///     let n = f.peek(&mut buffer[..])?;
  ///
  ///     println!("The bytes: {:?}", &buffer[..n]);
  ///     Ok(())
  /// }
  /// ```
  pub fn peek(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    let want_peek = buf.len();

    // check if the peek buffer has data
    let buffer_len = self.buffer.len();

    if buffer_len > 0 {
      return match want_peek.cmp(&buffer_len) {
        cmp::Ordering::Less => {
          buf.copy_from_slice(&self.buffer[..want_peek]);
          Ok(want_peek)
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(&self.buffer);
          Ok(want_peek)
        }
        cmp::Ordering::Greater => {
          let this = self;
          this.buffer.resize(want_peek, 0);
          match this.peeker.read(&mut this.buffer[buffer_len..]) {
            Ok(n) => {
              this.buffer.truncate(n + buffer_len);
              buf[..buffer_len + n].copy_from_slice(&this.buffer);
              Ok(buffer_len + n)
            }
            Err(e) => Err(e),
          }
        }
      };
    }

    let this = self;
    match this.peeker.read(buf) {
      Ok(bytes) => {
        this.buffer.extend_from_slice(&buf[..bytes]);
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
  /// [`File`]s implement `Peek`:
  ///
  /// [`peek()`]: Peek::peek
  /// [`Ok(0)`]: Ok
  /// [`File`]: crate::fs::File
  ///
  /// ```no_run
  /// use std::io;
  /// use std::io::prelude::*;
  /// use std::fs::File;
  /// use peekable::PeekExt;
  ///
  /// fn main() -> io::Result<()> {
  ///     let mut f = File::open("foo.txt")?.peekable();
  ///     let mut buffer = Vec::new();
  ///
  ///     // peek the whole file
  ///     f.peek_to_end(&mut buffer)?;
  ///     Ok(())
  /// }
  /// ```
  pub fn peek_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
    let this = &mut *self;
    let inbuf = this.buffer.len();

    let original_buf = buf.len();
    buf.extend_from_slice(&this.buffer);

    let fut = this.peeker.read_to_end(buf);
    match fut {
      Ok(read) => {
        this.buffer.extend_from_slice(&buf[original_buf + inbuf..]);
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
  /// ```no_run
  /// use std::io;
  /// use std::io::prelude::*;
  /// use std::fs::File;
  /// use peekable::PeekExt;
  ///
  /// fn main() -> io::Result<()> {
  ///     let mut f = File::open("foo.txt")?.peekable();
  ///     let mut buffer = String::new();
  ///
  ///     f.peek_to_string(&mut buffer)?;
  ///     Ok(())
  /// }
  /// ```
  pub fn peek_to_string(&mut self, buf: &mut String) -> Result<usize> {
    let s = match core::str::from_utf8(&self.buffer) {
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
    let fut = self.peeker.read_to_string(buf);
    match fut {
      Ok(read) => {
        self.buffer.extend_from_slice(&buf.as_bytes()[inbuf..]);
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
  /// ```no_run
  /// use std::io;
  /// use std::io::prelude::*;
  /// use std::fs::File;
  /// use peekable::PeekExt;
  ///
  /// fn main() -> io::Result<()> {
  ///     let mut f = File::open("foo.txt")?.peekable();
  ///     let mut buffer = [0; 10];
  ///
  ///     // peek exactly 10 bytes
  ///     f.peek_exact(&mut buffer)?;
  ///     Ok(())
  /// }
  /// ```
  pub fn peek_exact(&mut self, buf: &mut [u8]) -> Result<()> {
    let this = self;

    let buf_len = buf.len();
    let peek_buf_len = this.buffer.len();

    if buf_len <= peek_buf_len {
      buf.copy_from_slice(&this.buffer[..buf_len]);
      return Ok(());
    }

    buf[..buf_len].copy_from_slice(&this.buffer);
    this.read_exact(buf)
  }
}

/// An extension trait which adds utility methods to [`Read`] types.
pub trait PeekExt: Read {
  /// Wraps a [`Read`] type in a `Peekable` which provides a [`peek`] method.
  ///
  /// [`peek`]: Peek::peek
  fn peekable(self) -> Peekable<Self>
  where
    Self: Sized,
  {
    Peekable {
      peeker: self,
      buffer: Buffer::new(),
    }
  }
}

impl<R: Read + ?Sized> PeekExt for R {}
