use futures_util::{AsyncRead, AsyncReadExt, FutureExt};

use super::AsyncPeekable;
use std::{
  future::Future,
  io,
  pin::Pin,
  task::{Context, Poll},
};

/// Future for the [`peek_to_string`](super::AsyncPeekable::peek_to_string) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct PeekToString<'a, P> {
  reader: &'a mut AsyncPeekable<P>,
  buf: &'a mut String,
}

impl<P: Unpin> Unpin for PeekToString<'_, P> {}

impl<'a, P: AsyncRead + Unpin> PeekToString<'a, P> {
  pub(super) fn new(reader: &'a mut AsyncPeekable<P>, buf: &'a mut String) -> Self {
    Self { reader, buf }
  }
}

impl<A> Future for PeekToString<'_, A>
where
  A: AsyncRead + Unpin,
{
  type Output = io::Result<usize>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let Self { reader, buf } = &mut *self;
    let s = match core::str::from_utf8(&reader.buffer) {
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

    let inbuf = reader.buffer.len();
    let mut fut = reader.reader.read_to_string(buf);
    match fut.poll_unpin(cx) {
      Poll::Ready(Ok(read)) => {
        reader
          .buffer
          .extend_from_slice(&buf.as_bytes()[original_buf_len + inbuf..]);
        Poll::Ready(Ok(read + inbuf))
      }
      Poll::Ready(Err(e)) => {
        reader
          .buffer
          .extend_from_slice(&buf.as_bytes()[original_buf_len + inbuf..]);
        Poll::Ready(Err(e))
      }
      Poll::Pending => Poll::Pending,
    }
  }
}
