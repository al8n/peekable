use futures_util::AsyncRead;

use super::{AsyncPeek, AsyncPeekable, Buffer, DefaultBuffer};

use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek`](AsyncPeekable::peek) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Peek<'a, R, B = DefaultBuffer> {
  peekable: &'a mut AsyncPeekable<R, B>,
  buf: &'a mut [u8],
}

impl<R: Unpin, B> Unpin for Peek<'_, R, B> {}

impl<'a, R: AsyncRead + Unpin, B> Peek<'a, R, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<R, B>, buf: &'a mut [u8]) -> Self {
    Self { peekable, buf }
  }
}

impl<R: AsyncRead + Unpin, B: Buffer> Future for Peek<'_, R, B> {
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    Pin::new(&mut this.peekable).poll_peek(cx, this.buf)
  }
}
