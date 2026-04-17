use ::tokio::io::AsyncRead;
use pin_project_lite::pin_project;
use std::{
  future::Future,
  io,
  marker::PhantomPinned,
  pin::Pin,
  task::{Context, Poll},
};

use super::{AsyncPeekable, Buffer, DefaultBuffer};

pub(crate) fn peek_exact<'a, A, B>(
  peekable: &'a mut AsyncPeekable<A, B>,
  buf: &'a mut [u8],
) -> PeekExact<'a, A, B>
where
  A: AsyncRead + Unpin,
{
  PeekExact {
    peekable,
    buf,
    filled: 0,
    _pin: PhantomPinned,
  }
}

pin_project! {
  /// A future which can be used to easily read exactly enough bytes to fill
  /// a buffer.
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct PeekExact<'a, A, B = DefaultBuffer> {
    peekable: &'a mut AsyncPeekable<A, B>,
    buf: &'a mut [u8],
    filled: usize,
    #[pin]
    _pin: PhantomPinned,
  }
}

fn eof() -> io::Error {
  io::Error::new(io::ErrorKind::UnexpectedEof, "early eof")
}

impl<A, B> Future for PeekExact<'_, A, B>
where
  A: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
    let me = self.project();
    let total = me.buf.len();

    // On the first poll, copy what we can from the peek buffer.
    if *me.filled == 0 {
      let peek_buf = me.peekable.buffer.as_slice();
      let from_peek = peek_buf.len().min(total);
      me.buf[..from_peek].copy_from_slice(&peek_buf[..from_peek]);
      *me.filled = from_peek;

      if *me.filled == total {
        return Poll::Ready(Ok(total));
      }
    }

    // Read from the inner reader into the unfilled portion.
    while *me.filled < total {
      let mut read_buf = tokio::io::ReadBuf::new(&mut me.buf[*me.filled..]);
      ready!(Pin::new(&mut me.peekable.reader).poll_read(cx, &mut read_buf))?;
      let n = read_buf.filled().len();

      if n == 0 {
        return Poll::Ready(Err(eof()));
      }

      me.peekable
        .buffer
        .extend_from_slice(&me.buf[*me.filled..*me.filled + n])?;
      *me.filled += n;
    }

    Poll::Ready(Ok(total))
  }
}
