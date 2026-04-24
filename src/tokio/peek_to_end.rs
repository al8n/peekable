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
use crate::READ_CHUNK;

pin_project! {
  /// Peek to end
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct PeekToEnd<'a, R, B = DefaultBuffer> {
    peekable: &'a mut AsyncPeekable<R, B>,
    buf: &'a mut Vec<u8>,
    prefix_copied: bool,
    #[pin]
    _pin: PhantomPinned,
  }
}

pub(crate) fn peek_to_end<'a, R, B>(
  peekable: &'a mut AsyncPeekable<R, B>,
  buffer: &'a mut Vec<u8>,
) -> PeekToEnd<'a, R, B>
where
  R: AsyncRead + Unpin,
  B: Buffer,
{
  PeekToEnd {
    peekable,
    buf: buffer,
    prefix_copied: false,
    _pin: PhantomPinned,
  }
}

impl<A, B> Future for PeekToEnd<'_, A, B>
where
  A: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let me = self.project();

    if !*me.prefix_copied {
      me.buf.extend_from_slice(me.peekable.buffer.as_slice());
      *me.prefix_copied = true;
    }

    loop {
      let old_len = me.peekable.buffer.len();
      me.peekable.buffer.resize(old_len + READ_CHUNK)?;
      let mut tail = tokio::io::ReadBuf::new(&mut me.peekable.buffer.as_mut_slice()[old_len..]);
      match Pin::new(&mut me.peekable.reader).poll_read(cx, &mut tail) {
        Poll::Ready(Ok(())) => {
          let n = tail.filled().len();
          if n == 0 {
            me.peekable.buffer.truncate(old_len);
            return Poll::Ready(Ok(me.peekable.buffer.len()));
          }
          me.peekable.buffer.truncate(old_len + n);
          me.buf
            .extend_from_slice(&me.peekable.buffer.as_slice()[old_len..old_len + n]);
        }
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
          me.peekable.buffer.truncate(old_len);
          continue;
        }
        Poll::Ready(Err(e)) => {
          // Partial data already mirrored into both the peek buffer
          // (prior iterations) and `buf`. Leaving it in `buf` matches
          // std/tokio's read_to_end contract.
          me.peekable.buffer.truncate(old_len);
          return Poll::Ready(Err(e));
        }
        Poll::Pending => {
          me.peekable.buffer.truncate(old_len);
          return Poll::Pending;
        }
      }
    }
  }
}
