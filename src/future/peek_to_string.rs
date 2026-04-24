use futures_util::AsyncRead;

use super::{AsyncPeekable, Buffer, DefaultBuffer};
use crate::READ_CHUNK;
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
}

impl<P: Unpin, B> Unpin for PeekToString<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B: Buffer> PeekToString<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut String) -> Self {
    Self {
      peekable,
      buf,
      started: false,
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

    // Read directly into the peek buffer's tail so the reader only
    // advances for bytes recorded for replay. Accumulate the reader
    // outcome; validate UTF-8 after the loop so we can preserve the
    // longest valid prefix on an I/O error (matching std semantics).
    loop {
      let old_len = this.peekable.buffer.len();
      let loop_result: io::Result<()> =
        if let Err(e) = this.peekable.buffer.resize(old_len + READ_CHUNK) {
          Err(e)
        } else {
          match Pin::new(&mut this.peekable.reader)
            .poll_read(cx, &mut this.peekable.buffer.as_mut_slice()[old_len..])
          {
            Poll::Ready(Ok(0)) => {
              this.peekable.buffer.truncate(old_len);
              Ok(())
            }
            Poll::Ready(Ok(n)) => {
              this.peekable.buffer.truncate(old_len + n);
              continue;
            }
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
              this.peekable.buffer.truncate(old_len);
              continue;
            }
            Poll::Ready(Err(e)) => {
              this.peekable.buffer.truncate(old_len);
              Err(e)
            }
            Poll::Pending => {
              this.peekable.buffer.truncate(old_len);
              return Poll::Pending;
            }
          }
        };

      return Poll::Ready(
        match (
          loop_result,
          core::str::from_utf8(this.peekable.buffer.as_slice()),
        ) {
          (Ok(()), Ok(s)) => {
            this.buf.push_str(s);
            Ok(this.peekable.buffer.len())
          }
          (Ok(()), Err(e)) => Err(super::invalid_utf8_io_error(e)),
          (Err(io), Ok(s)) => {
            this.buf.push_str(s);
            Err(io)
          }
          (Err(io), Err(_)) => Err(io),
        },
      );
    }
  }
}
