use super::{AsyncPeekable, Buffer, DefaultBuffer};
use crate::grow_peek_buffer;
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
    started: bool,
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
  B: Buffer,
{
  PeekToString {
    peekable,
    output: string,
    started: false,
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

    if !*me.started {
      if let Err(e) = core::str::from_utf8(me.peekable.buffer.as_slice()) {
        if e.error_len().is_some() {
          return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, e)));
        }
      }
      *me.started = true;
    }

    // Read into the peek buffer's tail and validate UTF-8 only after
    // the reader signals EOF or an error — that way an I/O error still
    // preserves the longest valid-UTF-8 prefix of consumed bytes.
    loop {
      let old_len = me.peekable.buffer.len();
      let loop_result: io::Result<()> = match grow_peek_buffer(&mut me.peekable.buffer) {
        Err(e) => Err(e),
        Ok(growth) => {
          let mut tail = tokio::io::ReadBuf::new(
            &mut me.peekable.buffer.as_mut_slice()[old_len..old_len + growth],
          );
          match Pin::new(&mut me.peekable.reader).poll_read(cx, &mut tail) {
            Poll::Ready(Ok(())) => {
              let n = tail.filled().len();
              if n == 0 {
                me.peekable.buffer.truncate(old_len);
                Ok(())
              } else {
                me.peekable.buffer.truncate(old_len + n);
                continue;
              }
            }
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
              me.peekable.buffer.truncate(old_len);
              continue;
            }
            Poll::Ready(Err(e)) => {
              me.peekable.buffer.truncate(old_len);
              Err(e)
            }
            Poll::Pending => {
              me.peekable.buffer.truncate(old_len);
              return Poll::Pending;
            }
          }
        }
      };

      return Poll::Ready(
        match (
          loop_result,
          core::str::from_utf8(me.peekable.buffer.as_slice()),
        ) {
          (Ok(()), Ok(s)) => {
            me.output.push_str(s);
            Ok(me.peekable.buffer.len())
          }
          (Ok(()), Err(e)) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
          (Err(io), Ok(s)) => {
            me.output.push_str(s);
            Err(io)
          }
          (Err(io), Err(utf8_err)) => {
            // Preserve the longest valid-UTF-8 prefix of the
            // consumed bytes; bytes after `valid_up_to()` are either
            // invalid or an incomplete multi-byte sequence and are
            // dropped from `output` (still accessible via
            // `get_ref()`).
            let vut = utf8_err.valid_up_to();
            if vut != 0 {
              let s = core::str::from_utf8(&me.peekable.buffer.as_slice()[..vut])
                .expect("valid_up_to() must point to a valid UTF-8 prefix");
              me.output.push_str(s);
            }
            Err(io)
          }
        },
      );
    }
  }
}
