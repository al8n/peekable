use futures_util::AsyncRead;

use super::{AsyncPeekable, Buffer, DefaultBuffer};
use crate::StagingBuf;
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
  /// Position in `buf` where reader-provided data starts, or `None`
  /// if the peek-buffer prefix has not yet been copied into `buf`.
  reader_data_start: Option<usize>,
  /// Staging buffer for `poll_read`. Inline (via SmallVec or a
  /// fixed-size wrapper) so no separate heap allocation is needed
  /// for small reads.
  staging: StagingBuf,
}

impl<P: Unpin, B> Unpin for PeekToEnd<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B> PeekToEnd<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut Vec<u8>) -> Self {
    Self {
      peekable,
      buf,
      reader_data_start: None,
      staging: crate::new_staging_buf(),
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
    let inbuf = this.peekable.buffer.len();

    let reader_start = match this.reader_data_start {
      Some(pos) => pos,
      None => {
        this.buf.extend_from_slice(this.peekable.buffer.as_slice());
        let pos = this.buf.len();
        this.reader_data_start = Some(pos);
        pos
      }
    };

    loop {
      match Pin::new(&mut this.peekable.reader).poll_read(cx, &mut this.staging) {
        Poll::Ready(Ok(0)) => {
          this
            .peekable
            .buffer
            .extend_from_slice(&this.buf[reader_start..])?;
          return Poll::Ready(Ok(inbuf + (this.buf.len() - reader_start)));
        }
        Poll::Ready(Ok(n)) => {
          this.buf.extend_from_slice(&this.staging[..n]);
        }
        Poll::Ready(Err(e)) => {
          if this.buf.len() > reader_start {
            this
              .peekable
              .buffer
              .extend_from_slice(&this.buf[reader_start..])?;
          }
          return Poll::Ready(Err(e));
        }
        Poll::Pending => return Poll::Pending,
      }
    }
  }
}
