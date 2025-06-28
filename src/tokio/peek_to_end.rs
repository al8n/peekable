use ::tokio::io::{AsyncRead, AsyncReadExt};
use pin_project_lite::pin_project;

use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::{AsyncPeekable, Buffer, DefaultBuffer};

pin_project! {
  /// Peek to end
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct PeekToEnd<'a, R, B = DefaultBuffer> {
    peekable: &'a mut AsyncPeekable<R, B>,
    buf: &'a mut Vec<u8>,
    // Make this future `!Unpin` for compatibility with async trait methods.
    #[pin]
    _pin: PhantomPinned,
  }
}

pub(crate) fn peek_to_end<'a, R, B>(
  peekable: &'a mut AsyncPeekable<R, B>,
  buffer: &'a mut Vec<u8>,
) -> PeekToEnd<'a, R, B>
where
  R: AsyncRead + Unpin,
{
  PeekToEnd {
    peekable,
    buf: buffer,
    _pin: PhantomPinned,
  }
}

impl<A, B> Future for PeekToEnd<'_, A, B>
where
  A: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let me = self.project();

    let original_buf_len: usize = me.buf.len();
    let peek_buf_len = me.peekable.buffer.len();
    me.buf.extend_from_slice(me.peekable.buffer.as_slice());

    let fut = me.peekable.reader.read_to_end(me.buf);
    tokio::pin!(fut);
    match fut.poll(cx) {
      Poll::Ready(Ok(read)) => {
        me.peekable
          .buffer
          .extend_from_slice(&me.buf[original_buf_len + peek_buf_len..])?;
        Poll::Ready(Ok(peek_buf_len + read))
      }
      Poll::Ready(Err(e)) => {
        me.peekable
          .buffer
          .extend_from_slice(&me.buf[original_buf_len + peek_buf_len..])?;
        Poll::Ready(Err(e))
      }
      Poll::Pending => Poll::Pending,
    }
  }
}
