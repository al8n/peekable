use futures_util::AsyncRead;

use super::{AsyncPeekable, Buffer, DefaultBuffer};
use crate::StagingBuf;
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_to_string`](super::AsyncPeekable::peek_to_string) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekToString<'a, P, B = DefaultBuffer> {
  peekable: &'a mut AsyncPeekable<P, B>,
  buf: &'a mut String,
  /// `true` once the peek-buffer prefix has been validated.
  started: bool,
  /// Staging buffer for `poll_read` — inline for small reads.
  staging: StagingBuf,
}

impl<P: Unpin, B> Unpin for PeekToString<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B: Buffer> PeekToString<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut String) -> Self {
    Self {
      peekable,
      buf,
      started: false,
      staging: crate::new_staging_buf(),
    }
  }
}

impl<A, B> Future for PeekToString<'_, A, B>
where
  A: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;

    // Validate the existing peek-buffer prefix exactly once. Only
    // reject definitively invalid UTF-8 (`error_len().is_some()`);
    // an incomplete trailing sequence is allowed — the remaining
    // bytes may complete it.
    if !this.started {
      if let Err(e) = core::str::from_utf8(this.peekable.buffer.as_slice()) {
        if e.error_len().is_some() {
          return Poll::Ready(Err(super::invalid_utf8_io_error(e)));
        }
      }
      this.started = true;
    }

    // Read from the inner reader and accumulate directly into the
    // peek buffer — no separate `raw: Vec<u8>` needed. This keeps
    // peak memory at ~2× stream size (peek buffer + caller String)
    // instead of ~3× (peek buffer + raw + caller String).
    loop {
      match Pin::new(&mut this.peekable.reader).poll_read(cx, &mut this.staging) {
        Poll::Ready(Ok(0)) => {
          // EOF. Validate the full peek buffer as UTF-8.
          let s = match core::str::from_utf8(this.peekable.buffer.as_slice()) {
            Ok(s) => s,
            Err(e) => return Poll::Ready(Err(super::invalid_utf8_io_error(e))),
          };
          this.buf.push_str(s);
          return Poll::Ready(Ok(this.peekable.buffer.len()));
        }
        Poll::Ready(Ok(n)) => {
          this.peekable.buffer.extend_from_slice(&this.staging[..n])?;
        }
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
        Poll::Pending => return Poll::Pending,
      }
    }
  }
}
