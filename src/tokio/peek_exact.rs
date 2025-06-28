use ::tokio::io::{AsyncRead, ReadBuf};
use pin_project_lite::pin_project;
use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::{AsyncPeekable, Buffer, DefaultBuffer};

/// A future which can be used to easily read exactly enough bytes to fill
/// a buffer.
///
/// Created by the [`AsyncPeekExt::peek_exact`][peek_exact].
/// [peek_exact]: [super::AsyncPeekExt::peek_exact]
pub(crate) fn peek_exact<'a, A, B>(
  peekable: &'a mut AsyncPeekable<A, B>,
  buf: &'a mut [u8],
) -> PeekExact<'a, A, B>
where
  A: AsyncRead + Unpin,
{
  PeekExact {
    peekable,
    buf: ReadBuf::new(buf),
    _pin: PhantomPinned,
  }
}

pin_project! {
  /// Creates a future which will read exactly enough bytes to fill `buf`,
  /// returning an error if EOF is hit sooner.
  ///
  /// On success the number of bytes is returned
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct PeekExact<'a, A, B = DefaultBuffer> {
    peekable: &'a mut AsyncPeekable<A, B>,
    buf: ReadBuf<'a>,
    // Make this future `!Unpin` for compatibility with async trait methods.
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

    let buf_len = me.buf.remaining();
    let peek_buf_len = me.peekable.buffer.len();

    if buf_len <= peek_buf_len {
      me.buf.put_slice(&me.peekable.buffer.as_slice()[..buf_len]);
      return Poll::Ready(Ok(buf_len));
    }

    me.buf.put_slice(me.peekable.buffer.as_slice());
    let mut readed = me.peekable.buffer.len();

    while me.buf.remaining() != 0 {
      let before = me.buf.filled().len();
      ready!(Pin::new(&mut me.peekable.reader).poll_read(cx, me.buf))?;
      let after = me.buf.filled().len();
      let n = after - before;
      readed += n;
      me.peekable
        .buffer
        .extend_from_slice(&me.buf.filled()[before..after])?;

      if n == 0 && readed != buf_len {
        return Err(eof()).into();
      }
    }

    Poll::Ready(Ok(me.buf.capacity()))
  }
}
