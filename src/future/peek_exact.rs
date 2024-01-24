use futures_util::{AsyncRead, AsyncReadExt, FutureExt};

use super::AsyncPeekable;
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_exact`](super::AsyncPeekable::peek_exact) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekExact<'a, P> {
  peekable: &'a mut AsyncPeekable<P>,
  buf: &'a mut [u8],
}

impl<P: Unpin> Unpin for PeekExact<'_, P> {}

impl<'a, P: AsyncRead + Unpin> PeekExact<'a, P> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P>, buf: &'a mut [u8]) -> Self {
    Self { peekable, buf }
  }
}

impl<P: AsyncRead + Unpin> Future for PeekExact<'_, P> {
  type Output = io::Result<()>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;

    let buf_len = this.buf.len();
    let peek_buf_len = this.peekable.buffer.len();

    if buf_len <= peek_buf_len {
      this.buf.copy_from_slice(&this.peekable.buffer[..buf_len]);
      return Poll::Ready(Ok(()));
    }

    this.buf[..peek_buf_len].copy_from_slice(&this.peekable.buffer);
    let mut fut = this
      .peekable
      .read_exact(&mut this.buf[peek_buf_len..]);

    fut.poll_unpin(cx)
  }
}
