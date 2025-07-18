use futures_util::{io::IoSliceMut, AsyncRead};

use super::{AsyncPeek, AsyncPeekable, Buffer, DefaultBuffer};
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_vectored`](AsyncPeekable::peek_vectored) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekVectored<'a, R, B = DefaultBuffer> {
  peekable: &'a mut AsyncPeekable<R, B>,
  bufs: &'a mut [IoSliceMut<'a>],
}

impl<R: Unpin, B> Unpin for PeekVectored<'_, R, B> {}

impl<'a, R: AsyncRead + Unpin, B> PeekVectored<'a, R, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<R, B>, bufs: &'a mut [IoSliceMut<'a>]) -> Self {
    Self { peekable, bufs }
  }
}

impl<R: AsyncRead + Unpin, B: Buffer> Future for PeekVectored<'_, R, B> {
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    Pin::new(&mut this.peekable).poll_peek_vectored(cx, this.bufs)
  }
}
