use futures_util::{io::IoSliceMut, AsyncRead};

use super::{AsyncPeek, AsyncPeekable};
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_vectored`](AsyncPeekable::peek_vectored) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekVectored<'a, R> {
  peekable: &'a mut AsyncPeekable<R>,
  bufs: &'a mut [IoSliceMut<'a>],
}

impl<R: Unpin> Unpin for PeekVectored<'_, R> {}

impl<'a, R: AsyncRead + Unpin> PeekVectored<'a, R> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<R>, bufs: &'a mut [IoSliceMut<'a>]) -> Self {
    Self { peekable, bufs }
  }
}

impl<R: AsyncRead + Unpin> Future for PeekVectored<'_, R> {
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    Pin::new(&mut this.peekable).poll_peek_vectored(cx, this.bufs)
  }
}
