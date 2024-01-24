use std::{
  cmp, io,
  mem::MaybeUninit,
  ops::DerefMut,
  pin::Pin,
  task::{Context, Poll},
};

use super::Buffer;
use ::tokio::io::{AsyncRead, ReadBuf};

mod peek;
pub use peek::*;

mod peek_buf;
pub use peek_buf::*;

mod peek_exact;
pub use peek_exact::*;

mod peek_to_end;
pub use peek_to_end::*;

mod peek_to_string;
pub use peek_to_string::*;

/// Peeks bytes from a source.
///
/// This trait is analogous to the [`Peek`] trait, but integrates with
/// the asynchronous task system. In particular, the [`poll_peek`] method,
/// unlike [`Peek::peek`], will automatically queue the current task for wakeup
/// and return if data is not yet available, rather than blocking the calling
/// thpeek.
///
/// Specifically, this means that the `poll_peek` function will return one of
/// the following:
///
/// * `Poll::Ready(Ok(()))` means that data was immediately peek and placed into
///   the output buffer. The amount of data peek can be determined by the
///   increase in the length of the slice returned by `ReadBuf::filled`. If the
///   difference is 0, EOF has been reached.
///
/// * `Poll::Pending` means that no data was peek into the buffer
///   provided. The I/O object is not currently peekable but may become peekable
///   in the future. Most importantly, **the current future's task is scheduled
///   to get unparked when the object is peekable**. This means that like
///   `Future::poll` you'll receive a notification when the I/O object is
///   peekable again.
///
/// * `Poll::Ready(Err(e))` for other errors are standard I/O errors coming from the
///   underlying object.
///
/// This trait importantly means that the `peek` method only works in the
/// context of a future's task. The object may panic if used outside of a task.
///
/// Utilities for working with `AsyncPeek` values are provided by
/// [`AsyncPeekExt`].
///
/// [`poll_peek`]: AsyncPeek::poll_peek
/// [`Peek`]: crate::Peek
/// [`Peek::peek`]: create::Peek::peek
pub trait AsyncPeek: ::tokio::io::AsyncRead {
  /// Attempts to peek from the `AsyncPeek` into `buf`.
  ///
  /// On success, returns `Poll::Peeky(Ok(()))` and places data in the
  /// unfilled portion of `buf`. If no data was peek (`buf.filled().len()` is
  /// unchanged), it implies that EOF has been reached.
  ///
  /// If no data is available for peeking, the method returns `Poll::Pending`
  /// and arranges for the current task (via `cx.waker()`) to receive a
  /// notification when the object becomes peekable or is closed.
  fn poll_peek(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>>;
}

macro_rules! deref_async_peek {
  () => {
    fn poll_peek(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
      Pin::new(&mut **self).poll_peek(cx, buf)
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
  fn poll_peek(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    self.get_mut().as_mut().poll_peek(cx, buf)
  }
}

pin_project_lite::pin_project! {
  /// A wrapper around an [`AsyncRead`] types to make them support [`AsyncPeek`] methods.
  #[derive(Debug)]
  pub struct AsyncPeekable<R> {
    #[pin]
    peeker: R,
    buffer: Buffer,
  }
}

impl<R> From<R> for AsyncPeekable<R> {
  fn from(peeker: R) -> Self {
    Self {
      peeker,
      buffer: Buffer::new(),
    }
  }
}

impl<R: AsyncRead> AsyncRead for AsyncPeekable<R> {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    let this = self.project();
    let buffer_len = this.buffer.len();
    if buffer_len > 0 {
      let available = buf.remaining();

      return match available.cmp(&buffer_len) {
        cmp::Ordering::Greater => {
          // Continue peeking into the buffer if there's still space
          buf.put_slice(this.buffer);

          match this.peeker.poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
              this.buffer.clear();
              Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => {
              let len = buf.filled().len();
              // Safety: len - buffer_len..len guarantees we uninit the exact number of bytes from peek buffer
              unsafe {
                buf.inner_mut()[len - buffer_len..len].fill(MaybeUninit::uninit());
              }
              Poll::Ready(Err(e))
            }
            Poll::Pending => Poll::Pending,
          }
        }
        cmp::Ordering::Equal => {
          buf.put_slice(this.buffer);
          this.buffer.clear();
          return Poll::Ready(Ok(()));
        }
        cmp::Ordering::Less => {
          buf.put_slice(&this.buffer[..available]);
          this.buffer.drain(..available);
          return Poll::Ready(Ok(()));
        }
      };
    }

    this.peeker.poll_read(cx, buf)
  }
}

impl<R: AsyncRead> AsyncPeek for AsyncPeekable<R> {
  fn poll_peek(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    let this = self.project();
    let buffer_len = this.buffer.len();

    // Check if the buffer has enough data to fill `buf`
    if buffer_len > 0 {
      let available = buf.remaining();
      if available > buffer_len {
        // Not enough data in the buffer, need to peek more
        buf.put_slice(this.buffer);
        let cur = buf.filled().len();
        match this.peeker.poll_read(cx, buf) {
          Poll::Ready(Ok(())) => {
            let filled = buf.filled();
            let read = filled.len() - cur;
            this.buffer.extend_from_slice(&filled[cur..cur + read]);
            Poll::Ready(Ok(()))
          }
          Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
          Poll::Pending => {
            // Put what we have in the buffer into `buf` and return
            buf.put_slice(&this.buffer[..buffer_len]);
            Poll::Ready(Ok(()))
          }
        }
      } else {
        // Enough data in the buffer, copy it to `buf`
        buf.put_slice(&this.buffer[..available]);
        Poll::Ready(Ok(()))
      }
    } else {
      // No data in the buffer, try to peek directly into `buf`
      let cur = buf.filled().len();
      match this.peeker.poll_read(cx, buf) {
        Poll::Ready(Ok(())) => {
          let filled = buf.filled();
          let read = filled.len() - cur;
          this.buffer.extend_from_slice(&filled[cur..cur + read]);
          Poll::Ready(Ok(()))
        }
        Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
        Poll::Pending => Poll::Pending,
      }
    }
  }
}

impl<R> AsyncPeekable<R> {
  /// Creates a new `AsyncPeekable` which will wrap the given peeker.
  #[inline]
  pub fn new(reader: R) -> Self {
    Self {
      peeker: reader,
      buffer: Buffer::new(),
    }
  }

  /// Creates a new `AsyncPeekable` which will wrap the given peeker with the specified
  /// capacity for the peek buffer.
  #[inline]
  pub fn with_capacity(reader: R, capacity: usize) -> Self {
    Self {
      peeker: reader,
      buffer: Buffer::with_capacity(capacity),
    }
  }
}

/// An extension trait which adds peek related utility methods to [`AsyncRead`] types
pub trait AsyncPeekExt: AsyncRead {
  /// Creates a new `AsyncPeekable` which will wrap the given peeker.
  fn peekable(self) -> AsyncPeekable<Self>
  where
    Self: Sized,
  {
    AsyncPeekable {
      peeker: self,
      buffer: Buffer::new(),
    }
  }
}

impl<R: AsyncRead> AsyncPeekExt for R {}

impl<R: AsyncRead + Unpin> AsyncPeekable<R> {
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
  /// ```
  /// # #[tokio::main]
  /// # async fn main() -> std::io::Result<()> {
  /// use futures::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use tokio_util::compat::FuturesAsyncReadCompatExt;
  /// use peekable::tokio::AsyncPeekExt;
  ///
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).compat().peekable();
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
  /// # Ok::<(), std::io::Error>(())
  /// # }
  /// ```
  pub fn peek<'a>(&'a mut self, buf: &'a mut [u8]) -> Peek<'a, R>
  where
    Self: Unpin,
  {
    peek(self, buf)
  }

  /// Pulls some bytes from this source into the specified buffer,
  /// advancing the buffer's internal cursor.
  pub fn peek_buf<'a, B>(&'a mut self, buf: &'a mut B) -> PeekBuf<'a, R, B>
  where
    Self: Unpin,
    B: bytes::BufMut + ?Sized,
  {
    peek_buf(self, buf)
  }

  /// Creates a future which will peek exactly enough bytes to fill `buf`,
  /// returning an error if end of file (EOF) is hit sooner.
  ///
  /// The returned future will resolve once the read operation is completed.
  ///
  /// In the case of an error the buffer and the object will be discarded, with
  /// the error yielded.
  ///
  ///
  /// # Examples
  ///
  /// ```
  /// # #[tokio::main]
  /// # async fn main() -> std::io::Result<()> {
  /// use futures::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use tokio_util::compat::FuturesAsyncReadCompatExt;
  /// use peekable::tokio::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).compat().peekable();
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
  /// # Ok::<(), std::io::Error>(())
  /// # }
  /// ```
  ///
  /// ## EOF is hit before `buf` is filled
  ///
  /// ```
  /// # #[tokio::main]
  /// # async fn main() -> std::io::Result<()> {
  ///
  /// use futures::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use tokio_util::compat::FuturesAsyncReadCompatExt;
  /// use peekable::tokio::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).compat().peekable();
  /// let mut output = [0u8; 5];
  ///
  /// let result = peekable.peek_exact(&mut output).await;
  ///
  /// assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
  ///
  /// let result = peekable.peek_exact(&mut output[..4]).await;
  ///
  /// assert!(result.is_ok());
  /// assert_eq!(output, [1, 2, 3, 4, 0]);
  /// # Ok::<(), std::io::Error>(())
  /// # }
  /// ```
  pub fn peek_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> PeekExact<'a, R>
  where
    Self: Unpin,
  {
    peek_exact(self, buf)
  }

  /// Creates a future which will peek all the bytes from this `AsyncPeek`.
  ///
  /// On success the total number of bytes peek is returned.
  ///
  /// Equivalent to:
  ///
  /// ```ignore
  /// async fn peek_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize>;
  /// ```
  ///
  /// All bytes peek from this source will be appended to the specified
  /// buffer `buf`. This function will continuously call [`peek()`] to
  /// append more data to `buf` until [`peek()`] returns `Ok(0)`.
  ///
  /// If successful, the total number of bytes read is returned.
  ///
  /// [`peek()`]: AsyncPeekable::peek
  ///
  /// # Errors
  ///
  /// If a peek error is encountered then the `peek_to_end` operation
  /// immediately completes. Any bytes which have already been peek will
  /// be appended to `buf`.
  ///
  /// # Examples
  ///
  /// ```
  /// # #[tokio::main]
  /// # async fn main() -> std::io::Result<()> {
  /// use futures::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use tokio_util::compat::FuturesAsyncReadCompatExt;
  /// use peekable::tokio::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).compat().peekable();
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
  /// # Ok::<(), std::io::Error>(())
  /// # }
  /// ```
  pub fn peek_to_end<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> PeekToEnd<'a, R>
  where
    Self: Unpin,
  {
    peek_to_end(self, buf)
  }

  /// Creates a future which will peek all the bytes from this `AsyncPeek`.
  ///
  /// On success the total number of bytes peek is returned.
  ///
  /// # Examples
  ///
  /// ```
  /// # #[tokio::main]
  /// # async fn main() -> std::io::Result<()> {
  /// use futures::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use tokio_util::compat::FuturesAsyncReadCompatExt;
  /// use peekable::tokio::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new(&b"1234"[..]).compat().peekable();
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
  /// # Ok::<(), std::io::Error>(())
  /// # }
  /// ```
  pub fn peek_to_string<'a>(&'a mut self, dst: &'a mut String) -> PeekToString<'a, R>
  where
    Self: Unpin,
  {
    peek_to_string(self, dst)
  }
}
