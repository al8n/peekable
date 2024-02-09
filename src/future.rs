use std::{
  cmp,
  future::Future,
  io::Result,
  ops::DerefMut,
  pin::Pin,
  task::{Context, Poll},
};

use futures_util::{io::IoSliceMut, AsyncRead};

use super::*;

mod peek;
pub use peek::Peek;
mod peek_exact;
pub use peek_exact::PeekExact;
mod peek_to_string;
pub use peek_to_string::PeekToString;
mod peek_to_end;
pub use peek_to_end::PeekToEnd;
mod peek_vectored;
pub use peek_vectored::PeekVectored;

/// Peek bytes asynchronously.
///
/// This trait is analogous to the `peekable::Peek` trait, but integrates
/// with the asynchronous task system. In particular, the `poll_read`
/// method, unlike `Peek::peek`, will automatically queue the current task
/// for wakeup and return if data is not yet available, rather than blocking
/// the calling thread.
pub trait AsyncPeek: AsyncRead {
  /// Attempt to peek from the `AsyncPeek` into `buf`.
  ///
  /// On success, returns `Poll::Ready(Ok(num_bytes_peek))`.
  ///
  /// If no data is available for reading, the method returns
  /// `Poll::Pending` and arranges for the current task (via
  /// `cx.waker().wake_by_ref()`) to receive a notification when the object becomes
  /// readable or is closed.
  ///
  /// # Implementation
  ///
  /// This function may not return errors of kind `WouldBlock` or
  /// `Interrupted`.  Implementations must convert `WouldBlock` into
  /// `Poll::Pending` and either internally retry or convert
  /// `Interrupted` into another error kind.
  fn poll_peek(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize>>;

  /// Attempt to read from the `AsyncPeek` into `bufs` using vectored
  /// IO operations.
  ///
  /// This method is similar to `poll_peek`, but allows data to be read
  /// into multiple buffers using a single operation.
  ///
  /// On success, returns `Poll::Ready(Ok(num_bytes_read))`.
  ///
  /// If no data is available for reading, the method returns
  /// `Poll::Pending` and arranges for the current task (via
  /// `cx.waker().wake_by_ref()`) to receive a notification when the object becomes
  /// readable or is closed.
  /// By default, this method delegates to using `poll_read` on the first
  /// nonempty buffer in `bufs`, or an empty one if none exists. Objects which
  /// support vectored IO should override this method.
  ///
  /// # Implementation
  ///
  /// This function may not return errors of kind `WouldBlock` or
  /// `Interrupted`.  Implementations must convert `WouldBlock` into
  /// `Poll::Pending` and either internally retry or convert
  /// `Interrupted` into another error kind.
  fn poll_peek_vectored(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bufs: &mut [IoSliceMut<'_>],
  ) -> Poll<Result<usize>> {
    for b in bufs {
      if !b.is_empty() {
        return self.poll_peek(cx, b);
      }
    }

    self.poll_peek(cx, &mut [])
  }
}

macro_rules! deref_async_peek {
  () => {
    fn poll_peek(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      buf: &mut [u8],
    ) -> Poll<Result<usize>> {
      Pin::new(&mut **self).poll_peek(cx, buf)
    }

    fn poll_peek_vectored(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      bufs: &mut [IoSliceMut<'_>],
    ) -> Poll<Result<usize>> {
      Pin::new(&mut **self).poll_peek_vectored(cx, bufs)
    }
  };
}

impl<T: ?Sized + AsyncPeek + Unpin> AsyncPeek for Box<T> {
  deref_async_peek!();
}

impl<T: ?Sized + AsyncPeek + Unpin> AsyncPeek for &mut T {
  deref_async_peek!();
}

impl<P> AsyncPeek for Pin<P>
where
  P: DerefMut + Unpin,
  P::Target: AsyncPeek,
{
  fn poll_peek(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize>> {
    self.get_mut().as_mut().poll_peek(cx, buf)
  }

  fn poll_peek_vectored(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bufs: &mut [IoSliceMut<'_>],
  ) -> Poll<Result<usize>> {
    self.get_mut().as_mut().poll_peek_vectored(cx, bufs)
  }
}

pin_project_lite::pin_project! {
  /// A wrapper around an [`AsyncRead`] types to make them support [`AsyncPeek`] methods.
  #[derive(Debug)]
  pub struct AsyncPeekable<R> {
    #[pin]
    reader: R,
    buffer: Buffer,
  }
}

impl<R> From<R> for AsyncPeekable<R> {
  fn from(reader: R) -> Self {
    Self {
      reader,
      buffer: Buffer::new(),
    }
  }
}

impl<R: AsyncRead> AsyncRead for AsyncPeekable<R> {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    mut buf: &mut [u8],
  ) -> Poll<Result<usize>> {
    let want_read = buf.len();

    // check if the peek buffer has data
    let buffer_len = self.buffer.len();

    let this = self.project();
    if buffer_len > 0 {
      return match want_read.cmp(&buffer_len) {
        cmp::Ordering::Less => {
          buf.copy_from_slice(&this.buffer[..want_read]);
          this.buffer.drain(..want_read);
          return Poll::Ready(Ok(want_read));
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(this.buffer);
          this.buffer.clear();
          return Poll::Ready(Ok(want_read));
        }
        cmp::Ordering::Greater => {
          buf[..buffer_len].copy_from_slice(this.buffer);
          buf = &mut buf[buffer_len..];
          match this.reader.poll_read(cx, buf) {
            Poll::Ready(Ok(bytes)) => {
              this.buffer.clear();
              Poll::Ready(Ok(bytes + buffer_len))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => {
              this.buffer.clear();
              Poll::Ready(Ok(buffer_len))
            }
          }
        }
      };
    }

    this.reader.poll_read(cx, buf)
  }
}

impl<R: AsyncRead> AsyncPeek for AsyncPeekable<R> {
  fn poll_peek(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize>> {
    let want_peek = buf.len();

    // check if the peek buffer has data
    let buffer_len = self.buffer.len();

    if buffer_len > 0 {
      return match want_peek.cmp(&buffer_len) {
        cmp::Ordering::Less => {
          buf.copy_from_slice(&self.buffer[..want_peek]);
          Poll::Ready(Ok(want_peek))
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(&self.buffer);
          Poll::Ready(Ok(want_peek))
        }
        cmp::Ordering::Greater => {
          let this = self.project();
          this.buffer.resize(want_peek, 0);
          match this.reader.poll_read(cx, &mut this.buffer[buffer_len..]) {
            Poll::Ready(Ok(n)) => {
              this.buffer.truncate(n + buffer_len);
              buf[..buffer_len + n].copy_from_slice(this.buffer);
              Poll::Ready(Ok(buffer_len + n))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => {
              buf[..buffer_len].copy_from_slice(this.buffer);
              Poll::Ready(Ok(buffer_len))
            }
          }
        }
      };
    }

    let this = self.project();
    match this.reader.poll_read(cx, buf) {
      Poll::Ready(Ok(bytes)) => {
        this.buffer.extend_from_slice(&buf[..bytes]);
        Poll::Ready(Ok(bytes))
      }
      Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
      Poll::Pending => Poll::Pending,
    }
  }
}

impl<R> AsyncPeekable<R> {
  /// Creates a new `AsyncPeekable` which will wrap the given reader.
  #[inline]
  pub fn new(reader: R) -> Self {
    Self {
      reader,
      buffer: Buffer::new(),
    }
  }

  /// Creates a new peekable wrapper around the given reader with the specified
  /// capacity for the peek buffer.
  #[inline]
  pub fn with_capacity(reader: R, capacity: usize) -> Self {
    Self {
      reader,
      buffer: Buffer::with_capacity(capacity),
    }
  }

  /// Returns the bytes already be peeked into memory and a mutable reference to the underlying reader.
  ///
  /// **WARNING: If you invoke `AsyncRead` or `AsyncReadExt` methods on the underlying reader, may lead to unexpected read behaviors.**
  #[inline]
  pub fn get_mut(&mut self) -> (&[u8], &mut R) {
    (&self.buffer, &mut self.reader)
  }

  /// Returns the bytes already be peeked into memory and a reference to the underlying reader.
  ///
  /// **WARNING: If you invoke `AsyncRead` or `AsyncReadExt` methods on the underlying reader, may lead to unexpected read behaviors.**
  #[inline]
  pub fn get_ref(&self) -> (&[u8], &R) {
    (&self.buffer, &self.reader)
  }

  /// Consumes the `AsyncPeekable`, returning the a vec may contain the bytes already be peeked into memory and the wrapped reader.
  #[inline]
  pub fn into_components(self) -> (Buffer, R) {
    (self.buffer, self.reader)
  }
}

impl<R: AsyncRead + Unpin> AsyncPeekable<R> {
  /// Tries to peek some bytes directly into the given `buf` in asynchronous
  /// manner, returning a future type.
  ///
  /// The returned future will resolve to the number of bytes read once the read
  /// operation is completed.
  ///
  /// # Examples
  ///
  /// ```
  /// # futures::executor::block_on(async {
  /// use futures::io::{AsyncReadExt, Cursor};
  /// use peekable::future::AsyncPeekExt;
  ///
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = [0u8; 5];
  ///
  /// let bytes = peekable.peek(&mut output[..3]).await?;
  ///
  /// // This is only guaranteed to be 4 because `&[u8]` is a synchronous
  /// // reader. In a real system you could get anywhere from 1 to
  /// // `output.len()` bytes in a single read.
  /// assert_eq!(bytes, 3);
  /// assert_eq!(output, [1, 2, 3, 0, 0]);
  ///
  /// // you can peek mutiple times
  ///
  /// let bytes = peekable.peek(&mut output[..]).await?;
  /// assert_eq!(bytes, 4);
  /// assert_eq!(output, [1, 2, 3, 4, 0]);
  ///
  /// // you can read after peek
  /// let mut output = [0u8; 5];
  /// let bytes = peekable.read(&mut output[..2]).await?;
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2, 0, 0, 0]);
  ///
  /// // peek after read
  /// let mut output = [0u8; 5];
  /// let bytes = peekable.peek(&mut output[..]).await?;
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [3, 4, 0, 0, 0]);
  ///
  /// # Ok::<(), Box<dyn std::error::Error>>(()) }).unwrap();
  /// ```
  pub fn peek<'a>(&'a mut self, buf: &'a mut [u8]) -> Peek<'a, R>
  where
    Self: Unpin,
  {
    assert_future(Peek::new(self, buf))
  }

  /// Creates a future which will peek from the `AsyncPeek` into `bufs` using vectored
  /// IO operations.
  ///
  /// The returned future will resolve to the number of bytes read once the read
  /// operation is completed.
  pub fn peek_vectored<'a>(&'a mut self, bufs: &'a mut [IoSliceMut<'a>]) -> PeekVectored<'a, R>
  where
    Self: Unpin,
  {
    assert_future(PeekVectored::new(self, bufs))
  }

  /// Creates a future which will peek exactly enough bytes to fill `buf`,
  /// returning an error if end of file (EOF) is hit sooner.
  ///
  /// The returned future will resolve once the read operation is completed.
  ///
  /// In the case of an error the buffer and the object will be discarded, with
  /// the error yielded.
  ///
  /// # Examples
  ///
  /// ```rust
  /// # futures::executor::block_on(async {
  /// use futures::io::{AsyncReadExt, Cursor};
  /// use peekable::future::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = [0u8; 4];
  ///
  /// peekable.peek_exact(&mut output).await?;
  ///
  /// assert_eq!(output, [1, 2, 3, 4]);
  ///
  /// // read after peek
  /// let mut output = [0u8; 2];
  ///
  /// peekable.read_exact(&mut output[..]).await?;
  ///
  /// assert_eq!(output, [1, 2]);
  ///
  /// // peek after read
  /// let mut output = [0u8; 2];
  /// peekable.peek_exact(&mut output).await?;
  ///
  /// assert_eq!(output, [3, 4]);
  ///
  /// # Ok::<(), Box<dyn std::error::Error>>(()) }).unwrap();
  /// ```
  ///
  /// ## EOF is hit before `buf` is filled
  ///
  /// ```
  /// # futures::executor::block_on(async {
  /// use futures::io::{self, AsyncReadExt, Cursor};
  /// use peekable::future::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = [0u8; 5];

  /// let result = peekable.peek_exact(&mut output).await;
  /// assert_eq!(
  ///   result.unwrap_err().kind(),
  ///   std::io::ErrorKind::UnexpectedEof
  /// );

  /// let result = peekable.peek_exact(&mut output[..4]).await;
  /// assert!(result.is_ok());
  /// assert_eq!(output, [1, 2, 3, 4, 0]);
  /// # });
  /// ```
  pub fn peek_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> PeekExact<'a, R>
  where
    Self: Unpin,
  {
    assert_future::<Result<()>, _>(PeekExact::new(self, buf))
  }

  /// Creates a future which will peek all the bytes from this `AsyncPeek`.
  ///
  /// On success the total number of bytes peek is returned.
  ///
  /// # Examples
  ///
  /// ```
  /// # futures::executor::block_on(async {
  /// use futures::io::{AsyncReadExt, Cursor};
  /// use peekable::future::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
  /// let mut output = Vec::with_capacity(4);
  ///
  /// let bytes = peekable.peek_to_end(&mut output).await?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(output, vec![1, 2, 3, 4]);
  ///
  /// // read after peek
  /// let mut output = Vec::with_capacity(4);
  ///
  /// let bytes = peekable.read_to_end(&mut output).await?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(output, vec![1, 2, 3, 4]);
  ///
  /// # Ok::<(), Box<dyn std::error::Error>>(()) }).unwrap();
  /// ```
  pub fn peek_to_end<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> PeekToEnd<'a, R>
  where
    Self: Unpin,
  {
    assert_future::<Result<usize>, _>(PeekToEnd::new(self, buf))
  }

  /// Creates a future which will peek all the bytes from this `AsyncPeek`.
  ///
  /// On success the total number of bytes peek is returned.
  ///
  /// # Examples
  ///
  /// ```
  /// # futures::executor::block_on(async {
  /// use futures::io::{AsyncReadExt, Cursor};
  /// use peekable::future::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new(&b"1234"[..]).peekable();
  /// let mut buffer = String::with_capacity(4);
  ///
  /// let bytes = peekable.peek_to_string(&mut buffer).await?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(buffer, String::from("1234"));
  ///
  /// // read after peek
  /// let mut buffer = String::with_capacity(4);
  /// let bytes = peekable.peek_to_string(&mut buffer).await?;
  ///
  /// assert_eq!(bytes, 4);
  /// assert_eq!(buffer, String::from("1234"));
  ///
  /// # Ok::<(), Box<dyn std::error::Error>>(()) }).unwrap();
  /// ```
  pub fn peek_to_string<'a>(&'a mut self, buf: &'a mut String) -> PeekToString<'a, R>
  where
    Self: Unpin,
  {
    assert_future::<Result<usize>, _>(PeekToString::new(self, buf))
  }
}

/// An extension trait which adds peek related utility methods to [`AsyncRead`] types
pub trait AsyncPeekExt: AsyncRead {
  /// Creates a new `AsyncPeekable` which will wrap the given reader.
  fn peekable(self) -> AsyncPeekable<Self>
  where
    Self: Sized,
  {
    AsyncPeekable {
      reader: self,
      buffer: Buffer::new(),
    }
  }
}

impl<R: AsyncRead> AsyncPeekExt for R {}

// impl<R: AsyncPeek + ?Sized> AsyncPeekExt for R {}

// Just a helper function to ensure the futures we're returning all have the
// right implementations.
pub(crate) fn assert_future<T, F>(future: F) -> F
where
  F: Future<Output = T>,
{
  future
}

#[cfg(test)]
mod tests {
  use super::*;
  use futures::io::{AsyncReadExt, Cursor};

  #[test]
  fn test_peek_exact_peek_exact_read_exact() {
    futures::executor::block_on(async move {
      let mut peekable = Cursor::new([1, 2, 3, 4, 5, 6, 7, 8, 9]).peekable();
      let mut buf1 = [0; 2];
      peekable.peek_exact(&mut buf1).await.unwrap();
      assert_eq!(buf1, [1, 2]);

      let mut buf2 = [0; 4];
      peekable.peek_exact(&mut buf2).await.unwrap();
      assert_eq!(buf2, [1, 2, 3, 4]);

      let mut buf3 = [0; 4];
      peekable.read_exact(&mut buf3).await.unwrap();
      assert_eq!(buf3, [1, 2, 3, 4]);
    });
  }
}
