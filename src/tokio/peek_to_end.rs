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
use crate::StagingBuf;

pin_project! {
  /// Peek to end
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct PeekToEnd<'a, R, B = DefaultBuffer> {
    peekable: &'a mut AsyncPeekable<R, B>,
    buf: &'a mut Vec<u8>,
    reader_data_start: Option<usize>,
    staging: StagingBuf,
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
{
  PeekToEnd {
    peekable,
    buf: buffer,
    reader_data_start: None,
    staging: crate::new_staging_buf(),
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
    let inbuf = me.peekable.buffer.len();

    let reader_start = match *me.reader_data_start {
      Some(pos) => pos,
      None => {
        me.buf.extend_from_slice(me.peekable.buffer.as_slice());
        let pos = me.buf.len();
        *me.reader_data_start = Some(pos);
        pos
      }
    };

    loop {
      let mut read_buf = tokio::io::ReadBuf::new(me.staging);
      match Pin::new(&mut me.peekable.reader).poll_read(cx, &mut read_buf) {
        Poll::Ready(Ok(())) => {
          let n = read_buf.filled().len();
          if n == 0 {
            me.peekable
              .buffer
              .extend_from_slice(&me.buf[reader_start..])?;
            return Poll::Ready(Ok(inbuf + (me.buf.len() - reader_start)));
          }
          me.buf.extend_from_slice(read_buf.filled());
        }
        Poll::Ready(Err(e)) => {
          if me.buf.len() > reader_start {
            me.peekable
              .buffer
              .extend_from_slice(&me.buf[reader_start..])?;
          }
          return Poll::Ready(Err(e));
        }
        Poll::Pending => return Poll::Pending,
      }
    }
  }
}
