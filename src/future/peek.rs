use futures_util::AsyncRead;

use super::{AsyncPeek, AsyncPeekable};
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek`](AsyncPeekable::peek) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Peek<'a, R> {
  peekable: &'a mut AsyncPeekable<R>,
  buf: &'a mut [u8],
}

impl<R: Unpin> Unpin for Peek<'_, R> {}

impl<'a, R: AsyncRead + Unpin> Peek<'a, R> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<R>, buf: &'a mut [u8]) -> Self {
    Self { peekable, buf }
  }
}

impl<R: AsyncRead + Unpin> Future for Peek<'_, R> {
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    Pin::new(&mut this.peekable).poll_peek(cx, this.buf)
  }
}
