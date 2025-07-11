use std::{
  future::Future,
  ops::DerefMut,
  pin::Pin,
  task::{Context, Poll},
};

use futures_util::{AsyncRead, AsyncWrite};

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
mod fill_peek_buf;
pub use fill_peek_buf::FillPeekBuf;

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

impl<R, B> AsyncRead for AsyncPeekable<R, B>
where
  R: AsyncRead,
  B: Buffer,
{
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
          buf.copy_from_slice(&this.buffer.as_slice()[..want_read]);
          this.buffer.consume(..want_read);
          return Poll::Ready(Ok(want_read));
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(this.buffer.as_slice());
          this.buffer.clear();
          return Poll::Ready(Ok(want_read));
        }
        cmp::Ordering::Greater => {
          buf[..buffer_len].copy_from_slice(this.buffer.as_slice());
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

impl<W, B> AsyncWrite for AsyncPeekable<W, B>
where
  W: AsyncWrite,
{
  fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
    self.project().reader.poll_close(cx)
  }

  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
    self.project().reader.poll_flush(cx)
  }

  fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
    self.project().reader.poll_write(cx, buf)
  }
}

impl<R, B> AsyncPeek for AsyncPeekable<R, B>
where
  R: AsyncRead,
  B: Buffer,
{
  fn poll_peek(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize>> {
    let want_peek = buf.len();

    // check if the peek buffer has data
    let buffer_len = self.buffer.len();

    if buffer_len > 0 {
      return match want_peek.cmp(&buffer_len) {
        cmp::Ordering::Less => {
          buf.copy_from_slice(&self.buffer.as_slice()[..want_peek]);
          Poll::Ready(Ok(want_peek))
        }
        cmp::Ordering::Equal => {
          buf.copy_from_slice(self.buffer.as_slice());
          Poll::Ready(Ok(want_peek))
        }
        cmp::Ordering::Greater => {
          let this = self.project();
          this.buffer.resize(want_peek)?;
          match this
            .reader
            .poll_read(cx, &mut this.buffer.as_mut_slice()[buffer_len..])
          {
            Poll::Ready(Ok(n)) => {
              this.buffer.truncate(n + buffer_len);
              buf[..buffer_len + n].copy_from_slice(this.buffer.as_slice());
              Poll::Ready(Ok(buffer_len + n))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => {
              buf[..buffer_len].copy_from_slice(this.buffer.as_slice());
              Poll::Ready(Ok(buffer_len))
            }
          }
        }
      };
    }

    let this = self.project();
    match this.reader.poll_read(cx, buf) {
      Poll::Ready(Ok(bytes)) => {
        this.buffer.extend_from_slice(&buf[..bytes])?;
        Poll::Ready(Ok(bytes))
      }
      Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
      Poll::Pending => Poll::Pending,
    }
  }
}

impl<R> AsyncPeekable<R> {
  /// Creates a new `AsyncPeekable` which will wrap the given reader.
  ///
  /// The peek buffer will have the default capacity.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new(&b"hello"[..]);
  /// let peekable = AsyncPeekable::new(reader);
  /// # });
  ///
  #[inline]
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
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new([0; 1024]);
  /// let peekable = AsyncPeekable::with_capacity(reader, 1024);
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
  /// Creates a new `AsyncPeekable` which will wrap the given reader.
  /// This method allows you to specify a custom buffer type.
  ///
  /// The peek buffer will have the default capacity.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new(&b"hello"[..]);
  /// let peekable: AsyncPeekable<_, Vec<u8>> = AsyncPeekable::with_buffer(reader);
  /// # });
  ///
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

  /// Creates a new peekable wrapper around the given reader with the specified
  /// capacity for the peek buffer. This method allows you to specify a custom buffer type.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use peekable::future::AsyncPeekable;
  /// use std::vec::Vec;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new([0; 1024]);
  /// let peekable: AsyncPeekable<_, Vec<u8>> = AsyncPeekable::with_capacity_and_buffer(reader, 1024);
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
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new([1, 2, 3, 4]);
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
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new([1, 2, 3, 4]);
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
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable: AsyncPeekable<_> = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (buffer, reader) = peekable.get_mut();
  /// assert_eq!(buffer, [1, 2].as_slice());
  /// # });
  /// ````
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
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable: AsyncPeekable<_> = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (buffer, reader) = peekable.get_ref();
  /// assert_eq!(buffer, [1, 2].as_slice());
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
  /// use peekable::future::AsyncPeekable;
  ///
  /// # futures::executor::block_on(async {
  /// let reader = futures::io::Cursor::new([1, 2, 3, 4]);
  /// let mut peekable = AsyncPeekable::from(reader);
  ///
  /// let mut output = [0u8; 2];
  /// let bytes = peekable.peek(&mut output).await.unwrap();
  /// assert_eq!(bytes, 2);
  /// assert_eq!(output, [1, 2]);
  ///
  /// let (buffer, reader) = peekable.into_components();
  /// assert_eq!(buffer.as_slice(), [1, 2].as_slice());
  /// # });
  #[inline]
  pub fn into_components(self) -> (B, R) {
    (self.buffer, self.reader)
  }
}

impl<R, B> AsyncPeekable<R, B>
where
  R: AsyncRead + Unpin,
  B: Buffer,
{
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
  pub fn peek<'a>(&'a mut self, buf: &'a mut [u8]) -> Peek<'a, R, B>
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
  pub fn peek_vectored<'a>(&'a mut self, bufs: &'a mut [IoSliceMut<'a>]) -> PeekVectored<'a, R, B>
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
  ///
  /// let result = peekable.peek_exact(&mut output).await;
  /// assert_eq!(
  ///   result.unwrap_err().kind(),
  ///   std::io::ErrorKind::UnexpectedEof
  /// );
  ///
  /// let result = peekable.peek_exact(&mut output[..4]).await;
  /// assert!(result.is_ok());
  /// assert_eq!(output, [1, 2, 3, 4, 0]);
  /// # });
  /// ```
  pub fn peek_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> PeekExact<'a, R, B>
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
  pub fn peek_to_end<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> PeekToEnd<'a, R, B>
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
  /// // peek invalid utf-8
  /// let mut peekable = Cursor::new([255; 4]).peekable();
  /// let mut buffer = String::with_capacity(4);
  /// assert!(peekable.peek_to_string(&mut buffer).await.is_err());
  /// # Ok::<(), Box<dyn std::error::Error>>(()) }).unwrap();
  /// ```
  pub fn peek_to_string<'a>(&'a mut self, buf: &'a mut String) -> PeekToString<'a, R, B>
  where
    Self: Unpin,
  {
    assert_future::<Result<usize>, _>(PeekToString::new(self, buf))
  }

  /// Try to fill the peek buffer with more data. Returns the number of bytes peeked.
  ///
  /// # Examples
  ///
  /// ```rust
  /// use futures::io::Cursor;
  /// use futures::AsyncReadExt;
  /// use peekable::future::AsyncPeekExt;
  ///
  /// # futures::executor::block_on(async {
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
  pub fn fill_peek_buf(&mut self) -> FillPeekBuf<'_, R, B> {
    assert_future::<Result<usize>, _>(FillPeekBuf::new(self))
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

  /// Wraps a [`AsyncRead`] type in a `AsyncPeekable` which provides a `peek` related methods with a specified capacity.
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

  #[test]
  fn test_peek_exact_peek_exact_read_exact_1() {
    futures::executor::block_on(async move {
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
    });
  }

  #[test]
  fn test_peek_exact_peek_exact_read_exact_2() {
    futures::executor::block_on(async move {
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
    });
  }
}
