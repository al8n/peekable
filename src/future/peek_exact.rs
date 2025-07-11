use futures_util::AsyncRead;

use super::{AsyncPeekable, Buffer, DefaultBuffer};
use std::{
  future::Future,
  io, mem,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_exact`](super::AsyncPeekable::peek_exact) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekExact<'a, P, B = DefaultBuffer> {
  peekable: &'a mut AsyncPeekable<P, B>,
  buf: &'a mut [u8],
}

impl<P: Unpin, B> Unpin for PeekExact<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B> PeekExact<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut [u8]) -> Self {
    Self { peekable, buf }
  }
}

impl<P: AsyncRead + Unpin, B: Buffer> Future for PeekExact<'_, P, B> {
  type Output = io::Result<()>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;

    let buf_len = this.buf.len();
    let peek_buf_len = this.peekable.buffer.len();

    if buf_len <= peek_buf_len {
      this
        .buf
        .copy_from_slice(&this.peekable.buffer.as_slice()[..buf_len]);
      return Poll::Ready(Ok(()));
    }

    this.buf[..peek_buf_len].copy_from_slice(this.peekable.buffer.as_slice());
    {
      let (_, rest) = mem::take(&mut this.buf).split_at_mut(peek_buf_len);
      this.buf = rest;
    }

    let mut readed = peek_buf_len;
    while !this.buf.is_empty() {
      let n = ready!(Pin::new(&mut this.peekable.reader).poll_read(cx, this.buf))?;
      {
        let (read, rest) = mem::take(&mut this.buf).split_at_mut(n);
        this.peekable.buffer.extend_from_slice(read)?;
        readed += n;
        this.buf = rest;
      }
      if n == 0 && readed != buf_len {
        return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
      }
    }

    Poll::Ready(Ok(()))
  }
}
