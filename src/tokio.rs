use std::{
  cmp, io,
  mem::MaybeUninit,
  ops::DerefMut,
  pin::Pin,
  task::{Context, Poll},
};

use super::{Buffer, DefaultBuffer};
use ::tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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

mod fill_peek_buf;
pub use fill_peek_buf::*;

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
  pub struct AsyncPeekable<R, B = DefaultBuffer> {
    #[pin]
    reader: R,
    buffer: B,
    buf_cap: Option<usize>,
  }
}

impl<R> From<R> for AsyncPeekable<R> {
  fn from(reader: R) -> Self {
    Self::new(reader)
  }
}

impl<R> From<(usize, R)> for AsyncPeekable<R> {
  fn from((cap, reader): (usize, R)) -> Self {
    Self::with_capacity(reader, cap)
  }
}

impl<R: AsyncRead, B: Buffer> AsyncRead for AsyncPeekable<R, B> {
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
          buf.put_slice(this.buffer.as_slice());

          match this.reader.poll_read(cx, buf) {
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
          buf.put_slice(this.buffer.as_slice());
          this.buffer.clear();
          return Poll::Ready(Ok(()));
        }
        cmp::Ordering::Less => {
          buf.put_slice(&this.buffer.as_slice()[..available]);
          this.buffer.consume(..available);
          return Poll::Ready(Ok(()));
        }
      };
    }

    this.reader.poll_read(cx, buf)
  }
}

impl<W: AsyncWrite, B> AsyncWrite for AsyncPeekable<W, B> {
  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
    self.project().reader.poll_flush(cx)
  }

  fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
    self.project().reader.poll_shutdown(cx)
  }

  fn poll_write(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &[u8],
  ) -> Poll<Result<usize, io::Error>> {
    self.project().reader.poll_write(cx, buf)
  }
}

impl<R: AsyncRead, B: Buffer> AsyncPeek for AsyncPeekable<R, B> {
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
        buf.put_slice(this.buffer.as_slice());
        let cur = buf.filled().len();
        match this.reader.poll_read(cx, buf) {
          Poll::Ready(Ok(())) => {
            let filled = buf.filled();
            let read = filled.len() - cur;
            this.buffer.extend_from_slice(&filled[cur..cur + read])?;
            Poll::Ready(Ok(()))
          }
          Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
          Poll::Pending => {
            // Put what we have in the buffer into `buf` and return
            buf.put_slice(&this.buffer.as_slice()[..buffer_len]);
            Poll::Ready(Ok(()))
          }
        }
      } else {
        // Enough data in the buffer, copy it to `buf`
        buf.put_slice(&this.buffer.as_slice()[..available]);
        Poll::Ready(Ok(()))
      }
    } else {
      // No data in the buffer, try to peek directly into `buf`
      let cur = buf.filled().len();
      match this.reader.poll_read(cx, buf) {
        Poll::Ready(Ok(())) => {
          let filled = buf.filled();
          let read = filled.len() - cur;
          this.buffer.extend_from_slice(&filled[cur..cur + read])?;
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
  ///
  /// The peek buffer will have a capacity of 1024 bytes.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable = AsyncPeekable::from(reader);
  /// # });
  #[inline]
  pub fn new(reader: R) -> Self {
    Self {
      reader,
      buffer: DefaultBuffer::new(),
      buf_cap: None,
    }
  }

  /// Creates a new `AsyncPeekable` which will wrap the given peeker with the specified
  /// capacity for the peek buffer.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([0; 1024]);
  /// let mut peekable = AsyncPeekable::with_capacity(reader, 1024);
  /// # });
  #[inline]
  pub fn with_capacity(reader: R, capacity: usize) -> Self {
    Self {
      reader,
      buffer: DefaultBuffer::with_capacity(capacity),
      buf_cap: Some(capacity),
    }
  }
}

impl<R, B> AsyncPeekable<R, B> {
  /// Creates a new `AsyncPeekable` which will wrap the given peeker.
  /// This method allows you to specify a custom buffer type.
  ///
  /// The peek buffer will have a capacity of 1024 bytes.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable: AsyncPeekable<_, Vec<u8>> = AsyncPeekable::with_buffer(reader);
  /// # });
  #[inline]
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

  /// Creates a new `AsyncPeekable` which will wrap the given peeker with the specified
  /// capacity for the peek buffer.
  /// This method allows you to specify a custom buffer type.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([0; 1024]);
  /// let mut peekable: AsyncPeekable<_, Vec<u8>> = AsyncPeekable::with_capacity_and_buffer(reader, 1024);
  /// # });
  #[inline]
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

  /// Consumes the peek buffer and returns the buffer.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable: AsyncPeekable<_> = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let buffer = peekable.consume();
  /// assert_eq!(buffer.as_slice(), [1, 2].as_slice());
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [3, 4]);
  /// # });
  /// ```
  pub fn consume(&mut self) -> B
  where
    B: Buffer,
  {
    let buf = match self.buf_cap {
      Some(capacity) => B::with_capacity(capacity),
      None => B::new(),
    };
    core::mem::replace(&mut self.buffer, buf)
  }

  /// Consumes the peek buffer in place so that the peek buffer can be reused and avoid allocating.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable: AsyncPeekable<_> = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// peekable.consume_in_place();
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [3, 4]);
  /// # });
  /// ```
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
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable: AsyncPeekable<_> = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (buffer, reader) = peekable.get_mut();
  /// assert_eq!(buffer, [1, 2]);
  /// # });
  #[inline]
  pub fn get_mut(&mut self) -> (&[u8], &mut R)
  where
    B: Buffer,
  {
    (self.buffer.as_slice(), &mut self.reader)
  }

  /// Returns the bytes already be peeked into memory and a reference to the underlying reader.
  ///
  /// **WARNING: If you invoke `AsyncRead` or `AsyncReadExt` methods on the underlying reader, may lead to unexpected read behaviors.**
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable: AsyncPeekable<_> = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (buffer, reader) = peekable.get_ref();
  /// assert_eq!(buffer, [1, 2]);
  /// # });
  #[inline]
  pub fn get_ref(&self) -> (&[u8], &R)
  where
    B: Buffer,
  {
    (self.buffer.as_slice(), &self.reader)
  }

  /// Consumes the `AsyncPeekable`, returning the a vec may contain the bytes already be peeked into memory and the wrapped reader.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::tokio::AsyncPeekable;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  /// let reader = std::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (buffer, reader) = peekable.into_components();
  /// assert_eq!(buffer.as_slice(), [1, 2]);
  /// # });
  #[inline]
  pub fn into_components(self) -> (B, R) {
    (self.buffer, self.reader)
  }
}

/// An extension trait which adds peek related utility methods to [`AsyncRead`] types
pub trait AsyncPeekExt: AsyncRead {
  /// Creates a new `AsyncPeekable` which will wrap the given reader.
  fn peekable(self) -> AsyncPeekable<Self>
  where
    Self: Sized,
  {
    AsyncPeekable::from(self)
  }

  /// Wraps a [`Read`] type in a `Peekable` which provides a `peek` related methods with a specified capacity.
  fn peekable_with_capacity(self, capacity: usize) -> AsyncPeekable<Self>
  where
    Self: Sized,
  {
    AsyncPeekable::from((capacity, self))
  }

  /// Creates a new `AsyncPeekable` which will wrap the given reader.
  ///
  /// This method allows you to specify a custom buffer type.
  fn peekable_with_buffer<B>(self) -> AsyncPeekable<Self, B>
  where
    Self: Sized,
    B: Buffer,
  {
    AsyncPeekable::with_buffer(self)
  }

  /// Wraps a [`AsyncRead`] type in a `AsyncPeekable` which provides a `peek` related methods with a specified capacity.
  ///
  /// This method allows you to specify a custom buffer type.
  fn peekable_with_capacity_and_buffer<B>(self, capacity: usize) -> AsyncPeekable<Self, B>
  where
    Self: Sized,
    B: Buffer,
  {
    AsyncPeekable::with_capacity_and_buffer(self, capacity)
  }
}

impl<R: AsyncRead> AsyncPeekExt for R {}

impl<R: AsyncRead + Unpin, BUF: Buffer> AsyncPeekable<R, BUF> {
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
  /// use peekable::tokio::AsyncPeekExt;
  /// use tokio::io::AsyncReadExt;
  ///
  ///
  /// let mut peekable = std::io::Cursor::new([1, 2, 3, 4]).peekable();
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
  pub fn peek<'a>(&'a mut self, buf: &'a mut [u8]) -> Peek<'a, R, BUF>
  where
    Self: Unpin,
  {
    peek(self, buf)
  }

  /// Pulls some bytes from this source into the specified buffer,
  /// advancing the buffer's internal cursor.
  pub fn peek_buf<'a, B>(&'a mut self, buf: &'a mut B) -> PeekBuf<'a, R, B, BUF>
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
  /// use std::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use peekable::tokio::AsyncPeekExt;
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
  /// use std::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use peekable::tokio::AsyncPeekExt;
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable();
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
  pub fn peek_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> PeekExact<'a, R, BUF>
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
  /// use std::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use peekable::tokio::AsyncPeekExt;
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
  /// # Ok::<(), std::io::Error>(())
  /// # }
  /// ```
  pub fn peek_to_end<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> PeekToEnd<'a, R, BUF>
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
  /// use std::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use peekable::tokio::AsyncPeekExt;
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
  /// // peek invalid utf-8
  /// let mut peekable = Cursor::new([255; 4]).peekable();
  /// let mut buffer = String::with_capacity(4);
  /// assert!(peekable.peek_to_string(&mut buffer).await.is_err());
  /// # Ok::<(), std::io::Error>(())
  /// # }
  /// ```
  pub fn peek_to_string<'a>(&'a mut self, dst: &'a mut String) -> PeekToString<'a, R, BUF>
  where
    Self: Unpin,
  {
    peek_to_string(self, dst)
  }

  /// Try to fill the peek buffer with more data. Returns the number of bytes peeked.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use std::io::Cursor;
  /// use tokio::io::AsyncReadExt;
  /// use peekable::tokio::AsyncPeekExt;
  ///
  /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
  ///
  /// let mut peekable = Cursor::new([1, 2, 3, 4]).peekable_with_capacity(5);
  /// let mut output = [0u8; 4];
  ///
  /// peekable.peek_exact(&mut output[..1]).await.unwrap();
  /// assert_eq!(output, [1, 0, 0, 0]);
  ///
  /// let bytes = peekable.fill_peek_buf().await.unwrap();
  /// assert_eq!(bytes, 3);
  ///
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(output, [1, 2, 3, 4].as_slice());
  ///
  /// let readed = peekable.read(&mut output).await.unwrap();
  /// assert_eq!(readed, 4);
  /// # });
  /// ````
  pub fn fill_peek_buf(&mut self) -> FillPeekBuf<'_, R, BUF> {
    fill_peek_buf(self)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Cursor;
  use tokio::io::AsyncReadExt;

  #[tokio::test]
  async fn test_peek_exact_peek_exact_read_exact() {
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
  }

  #[tokio::test]
  async fn test_peek_exact_peek_exact_read_exact_1() {
    let mut peekable = Cursor::new([1, 2, 3, 4, 5, 6, 7, 8, 9]).peekable_with_buffer::<Vec<u8>>();
    let mut buf1 = [0; 2];
    peekable.peek_exact(&mut buf1).await.unwrap();
    assert_eq!(buf1, [1, 2]);

    let mut buf2 = [0; 4];
    peekable.peek_exact(&mut buf2).await.unwrap();
    assert_eq!(buf2, [1, 2, 3, 4]);

    let mut buf3 = [0; 4];
    peekable.read_exact(&mut buf3).await.unwrap();
    assert_eq!(buf3, [1, 2, 3, 4]);
  }

  #[tokio::test]
  async fn test_peek_exact_peek_exact_read_exact_2() {
    let mut peekable =
      Cursor::new([1, 2, 3, 4, 5, 6, 7, 8, 9]).peekable_with_capacity_and_buffer::<Vec<u8>>(24);
    let mut buf1 = [0; 2];
    peekable.peek_exact(&mut buf1).await.unwrap();
    assert_eq!(buf1, [1, 2]);

    let mut buf2 = [0; 4];
    peekable.peek_exact(&mut buf2).await.unwrap();
    assert_eq!(buf2, [1, 2, 3, 4]);

    let mut buf3 = [0; 4];
    peekable.read_exact(&mut buf3).await.unwrap();
    assert_eq!(buf3, [1, 2, 3, 4]);
  }
}
