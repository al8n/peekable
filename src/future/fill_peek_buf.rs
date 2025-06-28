use super::{AsyncPeekable, AsyncRead, Buffer, DefaultBuffer};

use pin_project_lite::pin_project;
use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

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

impl<'a, R, B: Buffer> FillPeekBuf<'a, R, B> {
  pub(super) fn new(peeker: &'a mut AsyncPeekable<R, B>) -> Self {
    let cap = peeker.buffer.capacity();
    let cur = peeker.buffer.len();
    Self {
      peeker,
      cap,
      original: cur,
      _pin: PhantomPinned,
    }
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

    let n = {
      ready!(Pin::new(&mut me.peeker.reader)
        .poll_read(cx, &mut me.peeker.buffer.as_mut_slice()[*me.original..])?)
    };

    me.peeker.buffer.truncate(*me.original + n);

    Poll::Ready(Ok(n))
  }
}
