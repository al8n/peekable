use super::{AsyncPeek, AsyncPeekable, AsyncRead, Buffer, DefaultBuffer};

use bytes::BufMut;
use pin_project_lite::pin_project;
use std::future::Future;
use std::io;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) fn peek_buf<'a, R, B, BUF>(
  peeker: &'a mut AsyncPeekable<R, BUF>,
  buf: &'a mut B,
) -> PeekBuf<'a, R, B, BUF>
where
  R: AsyncRead + Unpin,
  B: BufMut + ?Sized,
{
  PeekBuf {
    peeker,
    buf,
    _pin: PhantomPinned,
  }
}

pin_project! {
  /// Future returned by [`peek_buf`](crate::io::AsyncReadExt::peek_buf).
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct PeekBuf<'a, R, B: ?Sized, BUF = DefaultBuffer> {
    peeker: &'a mut AsyncPeekable<R, BUF>,
    buf: &'a mut B,
    #[pin]
    _pin: PhantomPinned,
  }
}

impl<R, B, BUF> Future for PeekBuf<'_, R, B, BUF>
where
  R: AsyncRead + Unpin,
  B: BufMut + ?Sized,
  BUF: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
    use ::tokio::io::ReadBuf;
    use std::mem::MaybeUninit;

    let me = self.project();

    if !me.buf.has_remaining_mut() {
      return Poll::Ready(Ok(0));
    }

    let n = {
      let dst = me.buf.chunk_mut();
      let dst = unsafe { &mut *(dst as *mut _ as *mut [MaybeUninit<u8>]) };
      let mut buf = ReadBuf::uninit(dst);
      let ptr = buf.filled().as_ptr();
      ready!(Pin::new(me.peeker).poll_peek(cx, &mut buf)?);

      // Ensure the pointer does not change from under us
      assert_eq!(ptr, buf.filled().as_ptr());
      buf.filled().len()
    };

    // Safety: This is guaranteed to be the number of initialized (and peek)
    // bytes due to the invariants provided by `PeekBuf::filled`.
    unsafe {
      me.buf.advance_mut(n);
    }

    Poll::Ready(Ok(n))
  }
}
