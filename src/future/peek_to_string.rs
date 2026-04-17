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
  /// Accumulator for raw bytes. UTF-8 validation is deferred to EOF
  /// so that partial multi-byte sequences across Pending boundaries
  /// don't cause spurious errors.
  raw: Vec<u8>,
  /// `true` once the peek-buffer prefix has been validated and
  /// appended to `raw`.
  prefix_copied: bool,
  /// Staging buffer for `poll_read` — inline for small reads.
  staging: StagingBuf,
}

impl<P: Unpin, B> Unpin for PeekToString<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B> PeekToString<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut String) -> Self {
    Self {
      peekable,
      buf,
      raw: Vec::new(),
      prefix_copied: false,
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
    let inbuf = this.peekable.buffer.len();

    if !this.prefix_copied {
      if let Err(e) = core::str::from_utf8(this.peekable.buffer.as_slice()) {
        return Poll::Ready(Err(super::invalid_utf8_io_error(e)));
      }
      this.raw.extend_from_slice(this.peekable.buffer.as_slice());
      this.prefix_copied = true;
    }

    loop {
      match Pin::new(&mut this.peekable.reader).poll_read(cx, &mut this.staging) {
        Poll::Ready(Ok(0)) => {
          // Mirror reader bytes into the peek buffer BEFORE the
          // UTF-8 check. The reader already consumed them; they
          // must survive even if the stream is invalid UTF-8.
          if this.raw.len() > inbuf {
            this.peekable.buffer.extend_from_slice(&this.raw[inbuf..])?;
          }

          let s = match core::str::from_utf8(&this.raw) {
            Ok(s) => s,
            Err(e) => return Poll::Ready(Err(super::invalid_utf8_io_error(e))),
          };
          this.buf.push_str(s);
          return Poll::Ready(Ok(this.raw.len()));
        }
        Poll::Ready(Ok(n)) => {
          this.raw.extend_from_slice(&this.staging[..n]);
        }
        Poll::Ready(Err(e)) => {
          if this.raw.len() > inbuf {
            let _ = this.peekable.buffer.extend_from_slice(&this.raw[inbuf..]);
          }
          return Poll::Ready(Err(e));
        }
        Poll::Pending => return Poll::Pending,
      }
    }
  }
}
