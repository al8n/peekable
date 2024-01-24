use ::tokio::io::{AsyncRead, ReadBuf};
use pin_project_lite::pin_project;
use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::marker::Unpin;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncReadExt;

use super::AsyncPeekable;

/// A future which can be used to easily read exactly enough bytes to fill
/// a buffer.
///
/// Created by the [`AsyncPeekExt::peek_exact`][peek_exact].
/// [peek_exact]: [super::AsyncPeekExt::peek_exact]
pub(crate) fn peek_exact<'a, A>(
  reader: &'a mut AsyncPeekable<A>,
  buf: &'a mut [u8],
) -> PeekExact<'a, A>
where
  A: AsyncRead + Unpin,
{
  PeekExact {
    reader,
    buf: ReadBuf::new(buf),
    _pin: PhantomPinned,
  }
}

pin_project! {
    /// Creates a future which will read exactly enough bytes to fill `buf`,
    /// returning an error if EOF is hit sooner.
    ///
    /// On success the number of bytes is returned
    #[derive(Debug)]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct PeekExact<'a, A> {
        reader: &'a mut AsyncPeekable<A>,
        buf: ReadBuf<'a>,
        // Make this future `!Unpin` for compatibility with async trait methods.
        #[pin]
        _pin: PhantomPinned,
    }
}

impl<A> Future for PeekExact<'_, A>
where
  A: AsyncRead + Unpin,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
    let me = self.project();

    let buf_len = me.buf.remaining();
    let peek_buf_len = me.reader.buffer.len();

    if buf_len <= peek_buf_len {
      me.buf.put_slice(&me.reader.buffer[..buf_len]);
      return Poll::Ready(Ok(buf_len));
    }

    me.buf.put_slice(&me.reader.buffer);

    let fut = me.reader.read_exact(me.buf.initialize_unfilled());
    ::tokio::pin!(fut);
    fut.poll(cx)
  }
}
