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
  /// Peek-buffer length at creation time. Stored once so the return
  /// value isn't inflated by chunks mirrored into the peek buffer
  /// during earlier polls.
  initial_peek_len: usize,
  /// Position in `buf` where reader-provided data starts, or `None`
  /// if the peek-buffer prefix has not yet been copied into `buf`.
  reader_data_start: Option<usize>,
  /// Staging buffer for `poll_read`. Inline (via SmallVec or a
  /// fixed-size wrapper) so no separate heap allocation is needed
  /// for small reads.
  staging: StagingBuf,
}

impl<P: Unpin, B> Unpin for PeekToEnd<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B: Buffer> PeekToEnd<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut Vec<u8>) -> Self {
    let initial_peek_len = peekable.buffer.len();
    Self {
      peekable,
      buf,
      initial_peek_len,
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
    let inbuf = this.initial_peek_len;

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
          // EOF — all chunks already mirrored incrementally.
          return Poll::Ready(Ok(inbuf + (this.buf.len() - reader_start)));
        }
        Poll::Ready(Ok(n)) => {
          let chunk = &this.staging[..n];
          // Mirror into the peek buffer first — if this fails, the
          // caller's buf stays clean (no partial append without a
          // matching peek-buffer entry).
          //
          // TODO(al8n): if `extend_from_slice` fails, the bytes in
          // `staging` are lost — the reader consumed them but they
          // can't be stored. A future improvement could read directly
          // into the peek buffer's tail (resize + poll_read into
          // buffer.as_mut_slice()[old_len..]) to eliminate this window.
          this.peekable.buffer.extend_from_slice(chunk)?;
          this.buf.extend_from_slice(chunk);
        }
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
        // Leave partial data in buf — matches std/tokio's
        // read_to_end contract.
        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
        Poll::Pending => return Poll::Pending,
      }
    }
  }
}
