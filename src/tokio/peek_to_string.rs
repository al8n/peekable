use super::{AsyncPeekable, Buffer, DefaultBuffer};
use crate::StagingBuf;
use ::tokio::io::AsyncRead;

use pin_project_lite::pin_project;
use std::{
  future::Future,
  io,
  marker::PhantomPinned,
  pin::Pin,
  task::{Context, Poll},
};

pin_project! {
  /// Future for the [`peek_to_string`](super::AsyncPeekExt::peek_to_string) method.
  #[derive(Debug)]
  #[must_use = "futures do nothing unless you `.await` or poll them"]
  pub struct PeekToString<'a, R, B = DefaultBuffer> {
    peekable: &'a mut AsyncPeekable<R, B>,
    output: &'a mut String,
    raw: Vec<u8>,
    prefix_copied: bool,
    staging: StagingBuf,
    #[pin]
    _pin: PhantomPinned,
  }
}

pub(crate) fn peek_to_string<'a, R, B>(
  peekable: &'a mut AsyncPeekable<R, B>,
  string: &'a mut String,
) -> PeekToString<'a, R, B>
where
  R: AsyncRead + Unpin,
{
  PeekToString {
    peekable,
    output: string,
    raw: Vec::new(),
    prefix_copied: false,
    staging: crate::new_staging_buf(),
    _pin: PhantomPinned,
  }
}

impl<A, B> Future for PeekToString<'_, A, B>
where
  A: AsyncRead + Unpin,
  B: Buffer,
{
  type Output = io::Result<usize>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let me = self.project();
    let inbuf = me.peekable.buffer.len();

    if !*me.prefix_copied {
      if let Err(e) = core::str::from_utf8(me.peekable.buffer.as_slice()) {
        if e.error_len().is_some() {
          return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, e)));
        }
      }
      me.raw.extend_from_slice(me.peekable.buffer.as_slice());
      *me.prefix_copied = true;
    }

    loop {
      let mut read_buf = tokio::io::ReadBuf::new(me.staging);
      match Pin::new(&mut me.peekable.reader).poll_read(cx, &mut read_buf) {
        Poll::Ready(Ok(())) => {
          let n = read_buf.filled().len();
          if n == 0 {
            // Mirror reader bytes into the peek buffer BEFORE the
            // UTF-8 check — consumed bytes must survive even if the
            // stream is invalid UTF-8.
            if me.raw.len() > inbuf {
              me.peekable.buffer.extend_from_slice(&me.raw[inbuf..])?;
            }

            let s = match core::str::from_utf8(me.raw) {
              Ok(s) => s,
              Err(e) => return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
            };
            me.output.push_str(s);
            return Poll::Ready(Ok(me.raw.len()));
          }
          me.raw.extend_from_slice(read_buf.filled());
        }
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
        Poll::Ready(Err(e)) => {
          if me.raw.len() > inbuf {
            me.peekable.buffer.extend_from_slice(&me.raw[inbuf..])?;
          }
          return Poll::Ready(Err(e));
        }
        Poll::Pending => return Poll::Pending,
      }
    }
  }
}
