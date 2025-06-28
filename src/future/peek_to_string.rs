use futures_util::{AsyncRead, AsyncReadExt, FutureExt};

use super::{AsyncPeekable, Buffer, DefaultBuffer};
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
}

impl<P: Unpin, B> Unpin for PeekToString<'_, P, B> {}

impl<'a, P: AsyncRead + Unpin, B> PeekToString<'a, P, B> {
  pub(super) fn new(peekable: &'a mut AsyncPeekable<P, B>, buf: &'a mut String) -> Self {
    Self { peekable, buf }
  }
}

impl<A, B> Future for PeekToString<'_, A, B>
where
  A: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let Self { peekable, buf } = &mut *self;
    let s = match core::str::from_utf8(peekable.buffer.as_slice()) {
      Ok(s) => s,
      Err(_) => {
        return Poll::Ready(Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "stream did not contain valid UTF-8",
        )))
      }
    };

    let original_buf_len = buf.len();
    buf.push_str(s);

    let inbuf = peekable.buffer.len();
    let mut fut = peekable.reader.read_to_string(buf);
    match fut.poll_unpin(cx) {
      Poll::Ready(Ok(read)) => {
        peekable
          .buffer
          .extend_from_slice(&buf.as_bytes()[original_buf_len + inbuf..])?;
        Poll::Ready(Ok(read + inbuf))
      }
      Poll::Ready(Err(e)) => {
        peekable
          .buffer
          .extend_from_slice(&buf.as_bytes()[original_buf_len + inbuf..])?;
        Poll::Ready(Err(e))
      }
      Poll::Pending => Poll::Pending,
    }
  }
}
