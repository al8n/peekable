use futures_util::AsyncRead;

use super::{AsyncPeekable, Buffer, DefaultBuffer};
use crate::grow_peek_buffer;
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
  /// `true` once the peek-buffer prefix has been copied into `buf`.
  prefix_copied: bool,
}

impl<P: Unpin, B> Unpin for PeekToEnd<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B: Buffer> PeekToEnd<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut Vec<u8>) -> Self {
    Self {
      peekable,
      buf,
      prefix_copied: false,
    }
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

    if !this.prefix_copied {
      this.buf.extend_from_slice(this.peekable.buffer.as_slice());
      this.prefix_copied = true;
    }

    loop {
      let old_len = this.peekable.buffer.len();
      let growth = grow_peek_buffer(&mut this.peekable.buffer)?;
      match Pin::new(&mut this.peekable.reader).poll_read(
        cx,
        &mut this.peekable.buffer.as_mut_slice()[old_len..old_len + growth],
      ) {
        Poll::Ready(Ok(0)) => {
          this.peekable.buffer.truncate(old_len);
          return Poll::Ready(Ok(this.peekable.buffer.len()));
        }
        Poll::Ready(Ok(n)) => {
          this.peekable.buffer.truncate(old_len + n);
          this
            .buf
            .extend_from_slice(&this.peekable.buffer.as_slice()[old_len..old_len + n]);
        }
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
          this.peekable.buffer.truncate(old_len);
          continue;
        }
        Poll::Ready(Err(e)) => {
          // Partial data already mirrored into both the peek buffer
          // (prior iterations) and `buf`. Leave it in `buf` — matches
          // std/tokio's `read_to_end` partial-data-on-error contract.
          this.peekable.buffer.truncate(old_len);
          return Poll::Ready(Err(e));
        }
        Poll::Pending => {
          this.peekable.buffer.truncate(old_len);
          return Poll::Pending;
        }
      }
    }
  }
}
