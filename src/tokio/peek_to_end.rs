use ::tokio::io::{AsyncRead, AsyncReadExt};
use pin_project_lite::pin_project;

use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::AsyncPeekable;

pin_project! {
    /// Peek to end
    #[derive(Debug)]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct PeekToEnd<'a, R> {
        reader: &'a mut AsyncPeekable<R>,
        buf: &'a mut Vec<u8>,
        // Make this future `!Unpin` for compatibility with async trait methods.
        #[pin]
        _pin: PhantomPinned,
    }
}

pub(crate) fn peek_to_end<'a, R>(
  reader: &'a mut AsyncPeekable<R>,
  buffer: &'a mut Vec<u8>,
) -> PeekToEnd<'a, R>
where
  R: AsyncRead + Unpin,
{
  PeekToEnd {
    reader,
    buf: buffer,
    _pin: PhantomPinned,
  }
}

impl<A> Future for PeekToEnd<'_, A>
where
  A: AsyncRead + Unpin,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let me = self.project();

    let original_buf_len = me.buf.len();
    let peek_buf_len = me.reader.buffer.len();
    me.buf.extend_from_slice(&me.reader.buffer);

    let fut = me.reader.peeker.read_to_end(me.buf);
    tokio::pin!(fut);
    match fut.poll(cx) {
      Poll::Ready(Ok(read)) => {
        me.reader
          .buffer
          .extend_from_slice(&me.buf[original_buf_len + peek_buf_len..]);
        Poll::Ready(Ok(peek_buf_len + read))
      }
      Poll::Ready(Err(e)) => {
        me.reader
          .buffer
          .extend_from_slice(&me.buf[original_buf_len + peek_buf_len..]);
        Poll::Ready(Err(e))
      }
      Poll::Pending => Poll::Pending,
    }
  }
}
