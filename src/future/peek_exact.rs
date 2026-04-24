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

    // Read from the inner reader directly into the peek buffer's
    // tail, then copy newly-produced bytes into the caller's buf.
    // The reader only advances for bytes already recorded for replay.
    while this.filled < total {
      let old_len = this.peekable.buffer.len();
      let want = total - this.filled;
      this.peekable.buffer.resize(old_len + want)?;

      let n = match Pin::new(&mut this.peekable.reader)
        .poll_read(cx, &mut this.peekable.buffer.as_mut_slice()[old_len..])
      {
        Poll::Ready(Ok(n)) => n,
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
          this.peekable.buffer.truncate(old_len);
          continue;
        }
        Poll::Ready(Err(e)) => {
          this.peekable.buffer.truncate(old_len);
          return Poll::Ready(Err(e));
        }
        Poll::Pending => {
          this.peekable.buffer.truncate(old_len);
          return Poll::Pending;
        }
      };

      this.peekable.buffer.truncate(old_len + n);
      if n == 0 {
        return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
      }
      this.buf[this.filled..this.filled + n]
        .copy_from_slice(&this.peekable.buffer.as_slice()[old_len..old_len + n]);
      this.filled += n;
    }

    Poll::Ready(Ok(()))
  }
}
