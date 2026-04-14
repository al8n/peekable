use super::{AsyncPeekable, AsyncRead, Buffer, DefaultBuffer};

use pin_project_lite::pin_project;
use std::{
  future::Future,
  io,
  marker::PhantomPinned,
  pin::Pin,
  task::{Context, Poll},
};
use tokio::io::ReadBuf;

pub(crate) fn fill_peek_buf<R, B>(peeker: &mut AsyncPeekable<R, B>) -> FillPeekBuf<'_, R, B>
where
  R: AsyncRead + Unpin,
  B: Buffer,
{
  let cap = peeker.buffer.capacity();
  let cur = peeker.buffer.len();
  FillPeekBuf {
    peeker,
    cap,
    original: cur,
    _pin: PhantomPinned,
  }
}

pin_project! {
  /// Future returned by [`peek_buf`](crate::io::AsyncReadExt::peek_buf).
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct FillPeekBuf<'a, R, B = DefaultBuffer> {
    peeker: &'a mut AsyncPeekable<R, B>,
    original: usize,
    cap: usize,
    #[pin]
    _pin: PhantomPinned,
  }
}

impl<R, B> Future for FillPeekBuf<'_, R, B>
where
  R: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
    let me = self.project();

    if me.cap == me.original {
      return Poll::Ready(Ok(0));
    }

    me.peeker.buffer.resize(*me.cap)?;

    let mut read = ReadBuf::new(&mut me.peeker.buffer.as_mut_slice()[*me.original..]);
    match Pin::new(&mut me.peeker.reader).poll_read(cx, &mut read) {
      Poll::Ready(Ok(())) => {
        let n = read.filled().len();
        me.peeker.buffer.truncate(*me.original + n);
        Poll::Ready(Ok(n))
      }
      Poll::Ready(Err(e)) => {
        // Roll back the resize so the peek buffer doesn't carry ghost
        // zero-bytes that a subsequent operation would treat as data.
        me.peeker.buffer.truncate(*me.original);
        Poll::Ready(Err(e))
      }
      Poll::Pending => {
        me.peeker.buffer.truncate(*me.original);
        Poll::Pending
      }
    }
  }
}
