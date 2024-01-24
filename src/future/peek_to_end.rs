use futures_util::{AsyncRead, AsyncReadExt, FutureExt};

use super::AsyncPeekable;
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_to_end`](super::AsyncPeekable::peek_to_end) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekToEnd<'a, P> {
  peekable: &'a mut AsyncPeekable<P>,
  buf: &'a mut Vec<u8>,
}

impl<P: Unpin> Unpin for PeekToEnd<'_, P> {}

impl<'a, P: AsyncRead + Unpin> PeekToEnd<'a, P> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P>, buf: &'a mut Vec<u8>) -> Self {
    Self { peekable, buf }
  }
}

impl<A> Future for PeekToEnd<'_, A>
where
  A: AsyncRead + Unpin,
{
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    let inbuf = this.peekable.buffer.len();

    let original_buf_len = this.buf.len();
    this.buf.extend_from_slice(&this.peekable.buffer);

    let mut fut = this.peekable.reader.read_to_end(this.buf);
    match fut.poll_unpin(cx) {
      Poll::Ready(Ok(read)) => {
        this.peekable.buffer.extend_from_slice(&this.buf[original_buf_len + inbuf..]);
        Poll::Ready(Ok(read + inbuf))
      }
      Poll::Ready(Err(e)) => {
        this.peekable.buffer.extend_from_slice(&this.buf[original_buf_len + inbuf..]);
        Poll::Ready(Err(e))
      }
      Poll::Pending => Poll::Pending,
    }
  }
}
