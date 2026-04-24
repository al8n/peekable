# Peek Buffer Correctness Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two correctness regressions in `Peekable` / `AsyncPeekable`:
1. `peek_to_string` discards partial valid UTF-8 on ordinary I/O errors, diverging from `std::io::Read::read_to_string`.
2. Several paths first consume bytes from the inner reader, then mirror them into the peek buffer via `Buffer::extend_from_slice`; if the fallible mirror fails, the reader has advanced but the peek buffer has not, breaking replay.

**Architecture:** Unify every "consume-then-mirror" code path onto a single pattern: **grow the peek buffer first, read directly into its tail, then truncate to the actual `n` on both success and error.** This makes the peek buffer the single source of truth for what the reader has produced, and naturally permits partial-data semantics. `peek_to_string` additionally checks UTF-8 validity of the full peek buffer and pushes the valid prefix to the caller's `String` on I/O error.

**Tech Stack:** Rust sync (`std::io::Read`), futures (`futures_util::AsyncRead`), tokio (`tokio::io::AsyncRead` + `ReadBuf`). Uses `Buffer` trait with fallible `resize` / `extend_from_slice`.

---

## File Structure

- `src/lib.rs` — sync `Peekable::peek` / `peek_exact` / `peek_to_end` / `peek_to_string` (+ existing unit tests at bottom).
- `src/future.rs` — `AsyncPeekable::poll_peek`.
- `src/future/peek_exact.rs`, `peek_to_end.rs`, `peek_to_string.rs` — async future state machines.
- `src/tokio.rs` — tokio `AsyncPeekable::poll_peek`.
- `src/tokio/peek_exact.rs`, `peek_to_end.rs`, `peek_to_string.rs` — tokio future state machines.
- `tests/sync.rs`, `tests/futures_io.rs`, `tests/tokio_io.rs` — append regression tests.
- `Cargo.toml` — version bump 0.6.0 → 0.6.1.
- `CHANGELOG.md` — describe fixes.

### Test support types (introduce once, reuse)

Both test helpers go at the top of each tests file (or a shared `tests/common/mod.rs`). Scope: used by new tests in this plan; keep `#[allow(dead_code)]` per helper if not used by every tests file.

```rust
// A Buffer whose extend_from_slice / resize fail once CAP bytes are
// exceeded. CAP is a const generic so tests pick a cap per scenario
// (the crate's constructor API calls Buffer::new() with no runtime
// argument, so a runtime cap field wouldn't plumb through).
#[derive(Debug, Default)]
pub struct BoundedBuffer<const CAP: usize> { inner: Vec<u8> }

impl<const CAP: usize> peekable::Buffer for BoundedBuffer<CAP> {
    fn new() -> Self { Self { inner: Vec::new() } }
    fn with_capacity(_: usize) -> Self { Self { inner: Vec::new() } }
    fn consume(&mut self, rng: std::ops::RangeTo<usize>) { self.inner.drain(rng); }
    fn clear(&mut self) { self.inner.clear(); }
    fn resize(&mut self, len: usize) -> std::io::Result<()> {
        if len > CAP {
            return Err(std::io::Error::new(std::io::ErrorKind::OutOfMemory, "BoundedBuffer cap"));
        }
        self.inner.resize(len, 0); Ok(())
    }
    fn truncate(&mut self, len: usize) { self.inner.truncate(len); }
    fn extend_from_slice(&mut self, other: &[u8]) -> std::io::Result<()> {
        if self.inner.len() + other.len() > CAP {
            return Err(std::io::Error::new(std::io::ErrorKind::OutOfMemory, "BoundedBuffer cap"));
        }
        self.inner.extend_from_slice(other); Ok(())
    }
    fn as_slice(&self) -> &[u8] { &self.inner }
    fn as_mut_slice(&mut self) -> &mut [u8] { &mut self.inner }
    fn len(&self) -> usize { self.inner.len() }
    fn is_empty(&self) -> bool { self.inner.is_empty() }
    fn capacity(&self) -> usize { CAP }
}

// Reader that hands out a fixed payload, then returns a custom io::Error.
#[derive(Debug)]
pub struct ErroringReader {
    data: std::io::Cursor<Vec<u8>>,
    pub error_after: usize, // total bytes to serve before erroring
    served: usize,
    pub kind: std::io::ErrorKind,
}

impl ErroringReader {
    pub fn new(data: Vec<u8>, error_after: usize, kind: std::io::ErrorKind) -> Self {
        Self { data: std::io::Cursor::new(data), error_after, served: 0, kind }
    }
}

impl std::io::Read for ErroringReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.served >= self.error_after {
            return Err(std::io::Error::new(self.kind, "synthetic"));
        }
        let cap = (self.error_after - self.served).min(buf.len());
        let n = std::io::Read::read(&mut self.data, &mut buf[..cap])?;
        self.served += n; Ok(n)
    }
}

// Async version uses futures_util::io::AsyncRead / tokio::io::AsyncRead
// — see the per-target tests file for wrappers.
```

---

## Canonical refactor shapes

These are the only two patterns every task applies.

### Pattern A — bounded read (for `peek`, `peek_exact`)

Given a desired extra byte count `want`:

```text
old_len = buffer.len();
buffer.resize(old_len + want)?;                      // fails cleanly if Buffer can't grow
match reader.read(&mut buffer.as_mut_slice()[old_len..]) {
    Ok(n)  => { buffer.truncate(old_len + n);         // n may be < want (short read)
                /* copy buffer[old_len..old_len+n] to caller slice */ }
    Err(e) => { buffer.truncate(old_len); return Err(e); }
}
```

For Interrupted retry: loop around the `reader.read` call (truncate + continue).

### Pattern B — unbounded chunked read (for `peek_to_end`, `peek_to_string`)

```text
let inbuf = buffer.len();
loop {
    let old_len = buffer.len();
    buffer.resize(old_len + CHUNK)?;
    match reader.read(&mut buffer.as_mut_slice()[old_len..]) {
        Ok(0)  => { buffer.truncate(old_len); break Ok(()); }
        Ok(n)  => { buffer.truncate(old_len + n); }
        Err(e) if e.kind() == Interrupted => { buffer.truncate(old_len); continue; }
        Err(e) => { buffer.truncate(old_len); break Err(e); }
    }
}
```

`CHUNK`: reuse existing `STAGING_CAP` = 1024 for async; for sync, define `const READ_CHUNK: usize = 1024;` near the top of `lib.rs`.

`peek_to_end` then copies `buffer.as_slice()[inbuf..]` into the caller's `Vec` at the end (or the existing peek-prefix + reader-data layout that the current code already uses). On `break Err(e)`, still copy whatever was accumulated in `buffer[inbuf..]` to caller `buf` (partial-data-on-error contract), then return the error.

`peek_to_string` at end:
```text
let validated = core::str::from_utf8(buffer.as_slice());
match (loop_result, validated) {
    (Ok(()), Ok(s))   => { caller_buf.push_str(s); Ok(buffer.len()) }
    (Ok(()), Err(e))  => Err(invalid_utf8_io_error(e)),           // caller_buf unchanged
    (Err(io), Ok(s))  => { caller_buf.push_str(s); Err(io) }       // partial-UTF-8 preserve
    (Err(io), Err(_)) => Err(io),                                   // caller_buf unchanged
}
```

Note: the existing peek-buffer prefix must still be validated up front (allowing incomplete trailing multi-byte sequences — same as current code).

---

## Task 1 — Plan-only prep

- [ ] **Step 1: Create a feature branch.**
  ```bash
  git checkout -b fix/peek-buffer-correctness
  ```

- [ ] **Step 2: Add `READ_CHUNK` constant and the two test-helper types.**

  In `src/lib.rs`, near the other internal consts:
  ```rust
  /// Chunk size used by sync `peek_to_end` / `peek_to_string`.
  const READ_CHUNK: usize = 1024;
  ```

  In `tests/sync.rs`, append `BoundedBuffer` and `ErroringReader` from the File Structure section above. (Do **not** add async wrappers yet — those come in Tasks 6, 8.)

- [ ] **Step 3: Commit.**
  ```bash
  git add -A && git commit -m "test(sync): add BoundedBuffer and ErroringReader helpers"
  ```

---

## Task 2 — Sync `peek` (Finding 2)

**File:** `src/lib.rs:548-606`

- [ ] **Step 1: Write regression test in `tests/sync.rs`.**

  ```rust
  #[test]
  fn peek_does_not_advance_reader_when_buffer_extend_fails() {
      use peekable::PeekExt;
      // Cap is 0 so even the first resize fails.
      let reader = std::io::Cursor::new(vec![1u8, 2, 3, 4]);
      let mut p = reader.peekable_with_buffer::<BoundedBuffer<0>>();
      let mut buf = [0u8; 2];
      let err = p.peek(&mut buf).expect_err("must fail");
      assert_eq!(err.kind(), std::io::ErrorKind::OutOfMemory);
      // Reader must not have advanced past what the peek buffer can
      // replay. Follow-up read should see all four bytes.
      let mut out = Vec::new();
      let (_, reader) = p.get_mut();
      std::io::Read::read_to_end(reader, &mut out).unwrap();
      assert_eq!(out, [1, 2, 3, 4]);
  }
  ```

  Note: `p.get_mut()` returns `(&[u8], &mut R)` per `src/lib.rs:382` — destructure to reach the inner reader.

- [ ] **Step 2: Run and confirm it fails.**
  ```bash
  cargo test --test sync peek_does_not_advance_reader_when_buffer_extend_fails -- --nocapture
  ```
  Expected: panic / assertion failure — today the reader advances past what the buffer can replay.

- [ ] **Step 3: Rewrite sync `peek` to use Pattern A.**

  Replace lib.rs:548-606 body. The three buffer-already-satisfies branches (Less / Equal) remain unchanged. The Greater branch already uses Pattern A — verify it still does after the rewrite and that `truncate(buffer_len)` happens on every `Err` path.

  For the final "no peek-buffer contents" branch (lib.rs:595-605):

  ```rust
  let this = self;
  let want = buf.len();
  if want == 0 {
      return Ok(0);
  }
  let old_len = this.buffer.len();        // == 0 on this path, but keep for symmetry
  this.buffer.resize(old_len + want)?;
  loop {
      match this.reader.read(&mut this.buffer.as_mut_slice()[old_len..]) {
          Ok(n) => {
              this.buffer.truncate(old_len + n);
              buf[..n].copy_from_slice(&this.buffer.as_slice()[old_len..old_len + n]);
              return Ok(n);
          }
          Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
          Err(e) => {
              this.buffer.truncate(old_len);
              return Err(e);
          }
      }
  }
  ```

- [ ] **Step 4: Confirm test passes + full sync suite still passes.**
  ```bash
  cargo test --test sync
  ```

- [ ] **Step 5: Commit.**
  ```bash
  git commit -am "fix(sync): read directly into peek buffer in peek() to preserve replay on Buffer error"
  ```

---

## Task 3 — Sync `peek_exact` (Finding 2)

**File:** `src/lib.rs:882-919`

- [ ] **Step 1: Write regression test.**

  ```rust
  #[test]
  fn peek_exact_does_not_advance_reader_when_buffer_extend_fails() {
      use peekable::PeekExt;
      let reader = std::io::Cursor::new(vec![1u8, 2, 3, 4]);
      // Cap 0 → first resize(+4) fails immediately.
      let mut p = reader.peekable_with_buffer::<BoundedBuffer<0>>();
      let mut buf = [0u8; 4];
      let err = p.peek_exact(&mut buf).expect_err("must fail");
      assert_eq!(err.kind(), std::io::ErrorKind::OutOfMemory);
      // Peek buffer contents + remaining reader bytes must == 4.
      let (peek_slice, reader) = p.get_mut();
      let have = peek_slice.len();
      let mut out = Vec::new();
      std::io::Read::read_to_end(reader, &mut out).unwrap();
      assert_eq!(have + out.len(), 4);
  }
  ```

  The invariant is `peek_buffer.len() + reader_remaining == 4`. `get_mut()` returns both simultaneously.

- [ ] **Step 2: Run and confirm failure.**
  ```bash
  cargo test --test sync peek_exact_does_not_advance_reader_when_buffer_extend_fails
  ```

- [ ] **Step 3: Rewrite the inner read loop using Pattern A.**

  Current body reads into caller `buf` then mirrors. Replace with: extend peek buffer by `remaining` bytes, read into its tail, truncate, then copy the bytes from the peek buffer into the caller's slice.

  ```rust
  pub fn peek_exact(&mut self, buf: &mut [u8]) -> Result<()> {
      let this = self;
      let total = buf.len();
      let peek_buf_len = this.buffer.len();

      if total <= peek_buf_len {
          buf.copy_from_slice(&this.buffer.as_slice()[..total]);
          return Ok(());
      }

      // Copy prefix from existing peek buffer.
      buf[..peek_buf_len].copy_from_slice(this.buffer.as_slice());
      let mut filled = peek_buf_len;

      while filled < total {
          let old_len = this.buffer.len();
          let want = total - filled;
          this.buffer.resize(old_len + want)?;

          let n = loop {
              match this.reader.read(&mut this.buffer.as_mut_slice()[old_len..]) {
                  Ok(n) => break n,
                  Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                  Err(e) => {
                      this.buffer.truncate(old_len);
                      return Err(e);
                  }
              }
          };
          this.buffer.truncate(old_len + n);
          if n == 0 {
              return Err(std::io::ErrorKind::UnexpectedEof.into());
          }
          buf[filled..filled + n]
              .copy_from_slice(&this.buffer.as_slice()[old_len..old_len + n]);
          filled += n;
      }
      Ok(())
  }
  ```

  Note: removes the `mem::take(&mut buf).split_at_mut(...)` dance — cleaner and no more `mem` import needed *if* nothing else uses it. Check lib.rs imports before removing.

- [ ] **Step 4: Run all sync tests.**
  ```bash
  cargo test --test sync
  cargo test --lib
  ```

- [ ] **Step 5: Commit.**
  ```bash
  git commit -am "fix(sync): read directly into peek buffer in peek_exact() to preserve replay on Buffer error"
  ```

---

## Task 4 — Sync `peek_to_end` (Finding 2, partial-data contract)

**File:** `src/lib.rs:675-703`

- [ ] **Step 1: Write regression tests.**

  ```rust
  #[test]
  fn peek_to_end_preserves_partial_data_on_io_error() {
      let reader = ErroringReader::new(b"hello".to_vec(), 3, std::io::ErrorKind::ConnectionReset);
      let mut p = peekable::PeekExt::peekable(reader);
      let mut out = Vec::new();
      let err = p.peek_to_end(&mut out).expect_err("io error expected");
      assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
      assert_eq!(out, b"hel");
  }

  #[test]
  fn peek_to_end_does_not_desync_on_buffer_error() {
      use peekable::PeekExt;
      let reader = std::io::Cursor::new(b"abcdef".to_vec());
      // Cap 2: first resize to old_len (0) + READ_CHUNK fails.
      // We pre-fill the peek buffer to 2 bytes first so resize target
      // becomes 2 + READ_CHUNK > 2 and fails on the peek_to_end chunk.
      let mut p = reader.peekable_with_buffer::<BoundedBuffer<2>>();
      let mut scratch = [0u8; 2];
      assert_eq!(p.peek(&mut scratch).unwrap(), 2);
      let mut out = Vec::new();
      let err = p.peek_to_end(&mut out).expect_err("must fail");
      assert_eq!(err.kind(), std::io::ErrorKind::OutOfMemory);
      // Invariant: peek buffer length + what reader still has == 6.
      let (peek_slice, reader) = p.get_mut();
      let have = peek_slice.len();
      let mut tail = Vec::new();
      std::io::Read::read_to_end(reader, &mut tail).unwrap();
      assert_eq!(have + tail.len(), 6);
  }
  ```

- [ ] **Step 2: Rewrite `peek_to_end` using Pattern B.**

  ```rust
  pub fn peek_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
      let this = &mut *self;
      let inbuf = this.buffer.len();
      let caller_start = buf.len();

      // Copy existing peek-buffer prefix into caller's buf up front.
      buf.extend_from_slice(this.buffer.as_slice());

      loop {
          let old_len = this.buffer.len();
          this.buffer.resize(old_len + READ_CHUNK)?;
          let n = match this.reader.read(&mut this.buffer.as_mut_slice()[old_len..]) {
              Ok(n) => n,
              Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                  this.buffer.truncate(old_len);
                  continue;
              }
              Err(e) => {
                  this.buffer.truncate(old_len);
                  // Preserve partial data in caller's buf: anything we
                  // mirrored into the peek buffer since this call began
                  // (i.e. buffer[inbuf..]) minus what we already copied
                  // (caller_start+inbuf up to caller_start+current len).
                  let already_in_caller = buf.len() - caller_start;
                  buf.extend_from_slice(&this.buffer.as_slice()[inbuf + (already_in_caller - inbuf)..]);
                  return Err(e);
              }
          };
          this.buffer.truncate(old_len + n);
          if n == 0 {
              return Ok(this.buffer.len()); // total length of accumulated content
          }
          buf.extend_from_slice(&this.buffer.as_slice()[old_len..old_len + n]);
      }
  }
  ```

  Wait — simplify. Because we copy chunks into `buf` as we mirror them, on Err we don't need any catch-up copy; `buf` already has everything we've successfully mirrored. The Err arm becomes:

  ```rust
          Err(e) => {
              this.buffer.truncate(old_len);
              return Err(e);
          }
  ```

  And the loop body only extends `buf` from the successful `n`-byte chunk. Use this simpler form in the implementation.

- [ ] **Step 3: Check return value.** Current signature returns `Result<usize>` — the count of "bytes peek". Existing code returns `read + inbuf` where `read` = count returned by `read_to_end`. Match that: return `this.buffer.len()` minus the value of `inbuf` at entry? No — current returns `read + inbuf`. `read` is the bytes the inner reader produced, `inbuf` is the pre-existing peek buffer length. After our loop, `this.buffer.len() == inbuf + (bytes read)`, so return that. But existing semantics is `read + inbuf` which equals `this.buffer.len() - 0` (since peek buffer started at `inbuf`). Same thing. Use `Ok(this.buffer.len())` after the `n == 0` branch only if `inbuf` was included; otherwise adjust.

  Safer: track `read_total` independently by accumulating `n`s, then return `read_total + inbuf` to exactly match the old semantics.

- [ ] **Step 4: Run tests.**
  ```bash
  cargo test --test sync peek_to_end
  cargo test --lib
  ```

- [ ] **Step 5: Commit.**
  ```bash
  git commit -am "fix(sync): chunked read-into-peek-buffer for peek_to_end; mirror bytes before yielding to caller"
  ```

---

## Task 5 — Sync `peek_to_string` (Findings 1 + 2)

**File:** `src/lib.rs:751-794`

- [ ] **Step 1: Write regression tests.**

  ```rust
  #[test]
  fn peek_to_string_preserves_partial_valid_utf8_on_io_error() {
      let reader = ErroringReader::new(b"hello".to_vec(), 3, std::io::ErrorKind::ConnectionReset);
      let mut p = peekable::PeekExt::peekable(reader);
      let mut out = String::new();
      let err = p.peek_to_string(&mut out).expect_err("io error expected");
      assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
      assert_eq!(out, "hel");
  }

  #[test]
  fn peek_to_string_leaves_buf_unchanged_when_partial_bytes_are_invalid_utf8() {
      // Emit the first two bytes of a 3-byte UTF-8 codepoint (0xE2 0x82 0xAC = €),
      // then error.
      let reader = ErroringReader::new(vec![0xE2, 0x82, 0xAC], 2, std::io::ErrorKind::ConnectionReset);
      let mut p = peekable::PeekExt::peekable(reader);
      let mut out = String::from("prefix:");
      let err = p.peek_to_string(&mut out).expect_err("io error expected");
      assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
      assert_eq!(out, "prefix:", "buf must be unchanged when accumulated bytes aren't valid UTF-8");
  }

  #[test]
  fn peek_to_string_returns_invalid_data_for_clean_invalid_utf8() {
      let reader = std::io::Cursor::new(vec![0xFF, 0xFE]);
      let mut p = peekable::PeekExt::peekable(reader);
      let mut out = String::from("prefix:");
      let err = p.peek_to_string(&mut out).expect_err("must fail");
      assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
      assert_eq!(out, "prefix:");
  }
  ```

- [ ] **Step 2: Rewrite `peek_to_string` using Pattern B + validity table.**

  ```rust
  pub fn peek_to_string(&mut self, buf: &mut String) -> Result<usize> {
      // Up-front: definitively invalid UTF-8 in the existing peek buffer
      // is a hard error; an incomplete trailing multi-byte sequence is
      // allowed (newly read bytes may complete it).
      if let Err(e) = core::str::from_utf8(self.buffer.as_slice()) {
          if e.error_len().is_some() {
              return Err(invalid_utf8_io_error(e));
          }
      }

      let loop_result: Result<()> = loop {
          let old_len = self.buffer.len();
          if self.buffer.resize(old_len + READ_CHUNK).is_err() {
              // A buffer-growth failure is itself an I/O error; treat it
              // the same as an I/O error from the reader — preserve the
              // partial valid-UTF-8 prefix.
              break Err(std::io::Error::new(
                  std::io::ErrorKind::OutOfMemory,
                  "peek buffer could not grow",
              ));
          }
          match self.reader.read(&mut self.buffer.as_mut_slice()[old_len..]) {
              Ok(0) => {
                  self.buffer.truncate(old_len);
                  break Ok(());
              }
              Ok(n) => {
                  self.buffer.truncate(old_len + n);
              }
              Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                  self.buffer.truncate(old_len);
                  continue;
              }
              Err(e) => {
                  self.buffer.truncate(old_len);
                  break Err(e);
              }
          }
      };

      match (loop_result, core::str::from_utf8(self.buffer.as_slice())) {
          (Ok(()), Ok(s)) => {
              buf.push_str(s);
              Ok(self.buffer.len())
          }
          (Ok(()), Err(e)) => Err(invalid_utf8_io_error(e)),
          (Err(io), Ok(s)) => {
              buf.push_str(s);
              Err(io)
          }
          (Err(io), Err(_)) => Err(io),
      }
  }
  ```

  **Doc update (lib.rs:710-716):** Replace the "# Errors" block to match std semantics:

  ```rust
  /// # Errors
  ///
  /// If the data in this stream is *not* valid UTF-8 then an error is
  /// returned and `buf` is unchanged.
  ///
  /// If an I/O error occurs, whatever bytes have been consumed so far
  /// remain in the internal peek buffer. Any valid-UTF-8 prefix of
  /// those bytes is appended to `buf` before the error is returned,
  /// matching [`std::io::Read::read_to_string`]'s contract.
  ```

- [ ] **Step 3: Run tests.**
  ```bash
  cargo test --test sync peek_to_string
  cargo test --lib
  ```

- [ ] **Step 4: Commit.**
  ```bash
  git commit -am "fix(sync): peek_to_string preserves partial valid UTF-8 on I/O error; std-compat"
  ```

---

## Task 6 — Futures async wrappers + test helpers

**Files:** `tests/futures_io.rs`

- [ ] **Step 1: Add `BoundedBuffer` (copy) and an async `ErroringReader` wrapper in `tests/futures_io.rs`.**

  ```rust
  struct AsyncErroringReader { inner: ErroringReader }
  impl futures_util::io::AsyncRead for AsyncErroringReader {
      fn poll_read(
          mut self: std::pin::Pin<&mut Self>,
          _: &mut std::task::Context<'_>,
          buf: &mut [u8],
      ) -> std::task::Poll<std::io::Result<usize>> {
          std::task::Poll::Ready(std::io::Read::read(&mut self.inner, buf))
      }
  }
  ```

- [ ] **Step 2: Commit.**
  ```bash
  git commit -am "test(futures): add async BoundedBuffer/ErroringReader helpers"
  ```

---

## Task 7 — Futures `poll_peek`, `peek_exact`, `peek_to_end`, `peek_to_string`

- [ ] **Step 1: Rewrite `src/future.rs:237-290` `poll_peek`.**

  Keep the "buffer already has data" branches intact. For both remaining paths (Greater + no-buffer), use Pattern A and ensure `truncate` on every error / Pending path where `resize` bumped the length. (Some paths already do this — verify after edits.)

- [ ] **Step 2: Rewrite `src/future/peek_exact.rs` using Pattern A.**

  In each `poll` iteration, track `old_len = this.peekable.buffer.len()`, `want = total - this.filled`, call `resize(old_len + want)?`, `poll_read` into the buffer tail, truncate on return, then copy from buffer tail to `this.buf[this.filled..]`. On `Pending`, `truncate(old_len)` before returning so a re-poll starts clean.

- [ ] **Step 3: Rewrite `src/future/peek_to_end.rs` using Pattern B.**

  Drop the `staging` field. Instead, loop: pre-resize, `poll_read` into the buffer tail, on Ok(0) truncate + finalize, on Ok(n) truncate to `old_len+n` and extend caller's `buf` by the new slice, on Err truncate + return (caller's `buf` already has everything mirrored so far). On Pending, truncate + return Pending so a re-poll starts clean.

- [ ] **Step 4: Rewrite `src/future/peek_to_string.rs` using Pattern B + validity table.**

  Drop `staging`. Up-front validation same as today. On Pending / Interrupted truncate the resize. On Err or EOF, apply the (loop_result, utf8_result) table in a single match.

- [ ] **Step 5: Tests in `tests/futures_io.rs`** — port the four test cases from the sync tasks, using block_on:

  ```rust
  #[test]
  fn peek_to_string_preserves_partial_valid_utf8_on_io_error() {
      use peekable::future::AsyncPeekExt;
      futures::executor::block_on(async {
          let reader = AsyncErroringReader { inner: ErroringReader::new(b"hello".to_vec(), 3, std::io::ErrorKind::ConnectionReset) };
          let mut p = reader.peekable();
          let mut out = String::new();
          let err = p.peek_to_string(&mut out).await.expect_err("io error");
          assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
          assert_eq!(out, "hel");
      });
  }
  // ... analogous tests for the other three cases and the two
  // fallible-buffer cases from Tasks 2/3/4.
  ```

- [ ] **Step 6: Run.**
  ```bash
  cargo test --test futures_io --features future
  ```

- [ ] **Step 7: Commit.**
  ```bash
  git commit -am "fix(futures): read-into-peek-buffer pattern; preserve partial UTF-8 on I/O error"
  ```

---

## Task 8 — Tokio async wrappers + test helpers

**Files:** `tests/tokio_io.rs`

- [ ] **Step 1: Add `BoundedBuffer` (copy) + an async `TokioErroringReader` that implements `tokio::io::AsyncRead` by doing sync read then setting `ReadBuf` filled length.**

- [ ] **Step 2: Commit helpers.**

---

## Task 9 — Tokio `poll_peek`, `peek_exact`, `peek_to_end`, `peek_to_string`

- [ ] **Step 1: Rewrite `src/tokio.rs:221-278` `poll_peek`.**

  Same two branches. `ReadBuf`-based reads: use `unfilled_mut()` on a wrapper over the peek buffer's tail or accept that tokio's `poll_read` writes into `buf: &mut ReadBuf`. The ergonomic move is:

  ```rust
  // Grow peek buffer by caller's remaining, wrap that tail in a ReadBuf,
  // poll_read, then copy the newly filled bytes into the caller's ReadBuf.
  let old_len = this.buffer.len();
  let want = caller_buf.remaining();
  this.buffer.resize(old_len + want)?;
  let mut tail = tokio::io::ReadBuf::new(&mut this.buffer.as_mut_slice()[old_len..]);
  match this.reader.poll_read(cx, &mut tail) {
      Poll::Ready(Ok(())) => {
          let n = tail.filled().len();
          this.buffer.truncate(old_len + n);
          caller_buf.put_slice(&this.buffer.as_slice()[old_len..old_len + n]);
          Poll::Ready(Ok(()))
      }
      Poll::Ready(Err(e)) => { this.buffer.truncate(old_len); Poll::Ready(Err(e)) }
      Poll::Pending => { this.buffer.truncate(old_len); Poll::Pending }
  }
  ```

  For the "buffer_len > 0 and want > buffer_len" branch, apply the same pattern but offset by `buffer_len` (the peek-buffer prefix is already put_slice'd into caller buf).

- [ ] **Step 2: Rewrite `src/tokio/peek_exact.rs` using Pattern A.** Same shape as futures version; use `ReadBuf` on the peek-buffer tail.

- [ ] **Step 3: Rewrite `src/tokio/peek_to_end.rs` using Pattern B.** Drop the `staging` field.

- [ ] **Step 4: Rewrite `src/tokio/peek_to_string.rs` using Pattern B + validity table.** Drop the `staging` field.

- [ ] **Step 5: Port tests to `tests/tokio_io.rs`.**
  ```bash
  cargo test --test tokio_io --features tokio
  ```

- [ ] **Step 6: Commit.**
  ```bash
  git commit -am "fix(tokio): read-into-peek-buffer pattern; preserve partial UTF-8 on I/O error"
  ```

---

## Task 10 — Version + changelog

- [ ] **Step 1: Bump `Cargo.toml` version 0.6.0 → 0.6.1.**

- [ ] **Step 2: Append to `CHANGELOG.md`:**

  ```markdown
  ## 0.6.1

  ### Fixed

  - `peek_to_string` (sync, futures, tokio) now preserves partial
    valid-UTF-8 text in the caller's `String` when the inner reader
    returns an I/O error, matching `std::io::Read::read_to_string`.
    The previous 0.6.0 behavior left `buf` unchanged on any error.
  - Fallible `Buffer` implementations can no longer desync the inner
    reader from the peek buffer. All read paths now grow the peek
    buffer and read directly into its tail; the reader only advances
    for bytes that are already recorded in the peek buffer.
  ```

- [ ] **Step 3: Final full-suite run.**
  ```bash
  cargo test --all-features
  cargo clippy --all-features --all-targets -- -D warnings
  cargo fmt --check
  cargo doc --all-features --no-deps
  ```

- [ ] **Step 4: Commit.**
  ```bash
  git commit -am "chore: release 0.6.1 with peek buffer correctness fixes"
  ```

---

## Notes on potentially-tricky details

- **Interrupted during Pattern B:** some readers return `Interrupted` even mid-stream. The loop body must `truncate(old_len)` and `continue` rather than leaving zero-filled bytes in the peek buffer.
- **`poll_read` spuriously yielding 0 bytes on a partial read:** async semantics allow 0-byte successful reads for certain edge cases. Treat `Ok(0)` in Pattern B as EOF (as today). If a reader can return `Ok(0)` erroneously, that's its bug.
- **Return value of `peek_to_end`:** must remain `read + inbuf` where `inbuf` = peek buffer length at entry. Pattern B's `this.buffer.len() - 0` at end equals that sum, but verify by inspection.
- **`mem` import in lib.rs:** `peek_exact` rewrite may remove the last use of `std::mem`. Remove the import if so — `cargo clippy -- -D warnings` will flag it.
- **Docs-only change** on `peek_to_string` is not considered a breaking API change; 0.6.1 patch bump is appropriate.
