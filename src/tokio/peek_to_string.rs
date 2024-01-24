use super::AsyncPeekable;
use ::tokio::io::{AsyncRead, AsyncReadExt};

use pin_project_lite::pin_project;
use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

pin_project! {
    /// Future for the [`peek_to_string`](super::AsyncPeekExt::peek_to_string) method.
    #[derive(Debug)]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct PeekToString<'a, R> {
        reader: &'a mut AsyncPeekable<R>,
        // This is the buffer we were provided. It will be replaced with an empty string
        // while reading to postpone utf-8 handling until after reading.
        output: &'a mut String,
        // Make this future `!Unpin` for compatibility with async trait methods.
        #[pin]
        _pin: PhantomPinned,
    }
}

pub(crate) fn peek_to_string<'a, R>(
  reader: &'a mut AsyncPeekable<R>,
  string: &'a mut String,
) -> PeekToString<'a, R>
where
  R: AsyncRead + Unpin,
{
  PeekToString {
    reader,
    output: string,
    _pin: PhantomPinned,
  }
}

impl<A> Future for PeekToString<'_, A>
where
  A: AsyncRead + Unpin,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let me = self.project();

    let s = match core::str::from_utf8(&me.reader.buffer) {
      Ok(s) => s,
      Err(e) => return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
    };

    let original_buf_len = me.output.len();
    let peek_buf_len = me.reader.buffer.len();
    me.output.push_str(s);
    let fut = me.reader.peeker.read_to_string(me.output);
    ::tokio::pin!(fut);
    match fut.poll(cx) {
      Poll::Ready(Ok(read)) => {
        me.reader
          .buffer
          .extend_from_slice(&me.output.as_bytes()[original_buf_len + peek_buf_len..]);
        Poll::Ready(Ok(peek_buf_len + read))
      }
      Poll::Ready(Err(e)) => {
        me.reader
          .buffer
          .extend_from_slice(&me.output.as_bytes()[original_buf_len + peek_buf_len..]);
        Poll::Ready(Err(e))
      },
      Poll::Pending => Poll::Pending,
    }
  }
}
