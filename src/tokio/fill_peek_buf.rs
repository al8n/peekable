use super::{AsyncPeekable, AsyncRead};

use pin_project_lite::pin_project;
use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::ReadBuf;

pub(crate) fn fill_peek_buf<R>(peeker: &mut AsyncPeekable<R>) -> FillPeekBuf<'_, R>
where
  R: AsyncRead + Unpin,
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
  pub struct FillPeekBuf<'a, R> {
    peeker: &'a mut AsyncPeekable<R>,
    original: usize,
    cap: usize,
    #[pin]
    _pin: PhantomPinned,
  }
}

impl<R> Future for FillPeekBuf<'_, R>
where
  R: AsyncRead + Unpin,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
    let me = self.project();

    if me.cap == me.original {
      return Poll::Ready(Ok(0));
    }

    me.peeker.buffer.resize(*me.cap, 0);

    let n = {
      let mut read = ReadBuf::new(&mut me.peeker.buffer[*me.original..]);
      ready!(Pin::new(&mut me.peeker.reader).poll_read(cx, &mut read)?);
      read.filled().len()
    };

    me.peeker.buffer.truncate(*me.original + n);

    Poll::Ready(Ok(n))
  }
}
