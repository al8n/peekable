use futures_util::{AsyncRead, AsyncReadExt, FutureExt};

use super::{AsyncPeekable, Buffer, DefaultBuffer};
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_to_end`](super::AsyncPeekable::peek_to_end) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekToEnd<'a, P, B = DefaultBuffer> {
  peekable: &'a mut AsyncPeekable<P, B>,
  buf: &'a mut Vec<u8>,
}

impl<P: Unpin, B> Unpin for PeekToEnd<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B> PeekToEnd<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut Vec<u8>) -> Self {
    Self { peekable, buf }
  }
}

impl<A, B> Future for PeekToEnd<'_, A, B>
where
  A: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    let inbuf = this.peekable.buffer.len();

    let original_buf_len = this.buf.len();
    this.buf.extend_from_slice(this.peekable.buffer.as_slice());

    let mut fut = this.peekable.reader.read_to_end(this.buf);
    match fut.poll_unpin(cx) {
      Poll::Ready(Ok(read)) => {
        this
          .peekable
          .buffer
          .extend_from_slice(&this.buf[original_buf_len + inbuf..])?;
        Poll::Ready(Ok(read + inbuf))
      }
      Poll::Ready(Err(e)) => {
        this
          .peekable
          .buffer
          .extend_from_slice(&this.buf[original_buf_len + inbuf..])?;
        Poll::Ready(Err(e))
      }
      Poll::Pending => Poll::Pending,
    }
  }
}
