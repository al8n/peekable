use futures_util::AsyncRead;

use super::{AsyncPeekable, Buffer, DefaultBuffer};
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_exact`](super::AsyncPeekable::peek_exact) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekExact<'a, P, B = DefaultBuffer> {
  peekable: &'a mut AsyncPeekable<P, B>,
  buf: &'a mut [u8],
  /// How many bytes of `buf` have been filled so far (from the peek
  /// buffer prefix + inner reader). Survives across Pending polls.
  filled: usize,
}

impl<P: Unpin, B> Unpin for PeekExact<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B> PeekExact<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut [u8]) -> Self {
    Self {
      peekable,
      buf,
      filled: 0,
    }
  }
}

impl<P: AsyncRead + Unpin, B: Buffer> Future for PeekExact<'_, P, B> {
  type Output = io::Result<()>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    let total = this.buf.len();

    // On the first poll, copy what we can from the peek buffer.
    if this.filled == 0 {
      let peek_buf = this.peekable.buffer.as_slice();
      let from_peek = peek_buf.len().min(total);
      this.buf[..from_peek].copy_from_slice(&peek_buf[..from_peek]);
      this.filled = from_peek;

      if this.filled == total {
        return Poll::Ready(Ok(()));
      }
    }

    // Read from the inner reader into the unfilled portion of `buf`.
    while this.filled < total {
      let n = match Pin::new(&mut this.peekable.reader).poll_read(cx, &mut this.buf[this.filled..])
      {
        Poll::Ready(Ok(n)) => n,
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
        Poll::Pending => return Poll::Pending,
      };

      if n == 0 {
        return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
      }

      // Mirror the newly-read bytes into the peek buffer so the
      // peek abstraction is maintained.
      //
      // TODO(al8n): if `extend_from_slice` fails, the bytes are in
      // `buf` but not in the peek buffer — breaking the abstraction
      // for custom Buffer impls. Same future-improvement as noted in
      // peek_to_end.rs and peek_to_string.rs.
      this
        .peekable
        .buffer
        .extend_from_slice(&this.buf[this.filled..this.filled + n])?;
      this.filled += n;
    }

    Poll::Ready(Ok(()))
  }
}
