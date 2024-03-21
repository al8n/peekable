use ::tokio::io::{AsyncRead, ReadBuf};
use pin_project_lite::pin_project;

use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::{AsyncPeek, AsyncPeekable};

/// Tries to read some bytes directly into the given `buf` in asynchronous
/// manner, returning a future type.
///
/// The returned future will resolve to both the I/O stream and the buffer
/// as well as the number of bytes read once the read operation is completed.
pub(crate) fn peek<'a, R>(peekable: &'a mut AsyncPeekable<R>, buf: &'a mut [u8]) -> Peek<'a, R>
where
  R: AsyncRead + Unpin,
{
  Peek {
    peekable,
    buf,
    _pin: PhantomPinned,
  }
}

pin_project! {
  /// A future which can be used to easily read available number of bytes to fill
  /// a buffer.
  ///
  /// Created by the [`peek`] function.
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct Peek<'a, R> {
    peekable: &'a mut AsyncPeekable<R>,
    buf: &'a mut [u8],
    // Make this future `!Unpin` for compatibility with async trait methods.
    #[pin]
    _pin: PhantomPinned,
  }
}

impl<R> Future for Peek<'_, R>
where
  R: AsyncRead + Unpin,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
    let me = self.project();
    let mut buf = ReadBuf::new(me.buf);
    ready!(Pin::new(me.peekable).poll_peek(cx, &mut buf))?;
    Poll::Ready(Ok(buf.filled().len()))
  }
}
