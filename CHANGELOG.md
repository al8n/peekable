# CHANGELOG

## UNRELEASED

### 0.5.1

#### Fixed (correctness — async futures + tokio)

- **`peek_to_end` / `peek_to_string` / `peek_exact`**: all three async
  futures (both `future::` and `tokio::` variants) recreated their inner
  read future on every `poll`, losing partial-read progress across
  `Pending` boundaries. `peek_to_end` duplicated the peek-buffer prefix
  each time; `peek_to_string` lost the internal UTF-8 progress state;
  `peek_exact` copied from offset 0 of the peek buffer on re-poll
  instead of the continuation offset (`abcd` → `abab`). All six futures
  are now proper state machines with progress fields (`reader_data_start`,
  `started`, `filled`) that survive `Pending`. The error-path semantics
  now match `std::io::Read` contracts: `peek_to_end` leaves any partial
  data appended to the caller's `Vec` in place on error (matching
  `read_to_end`), while `peek_to_string` leaves the caller's `String`
  unchanged on error (matching `read_to_string`).

#### Fixed (correctness — sync)

- **`peek_to_string` buffer corruption with non-empty `String`**: the
  offset used to mirror reader bytes into the peek buffer was `inbuf`
  (peek-buffer size) instead of `original_buf_len + inbuf`, so a caller
  passing a non-empty String would inject the caller's own prefix into
  the peek buffer. Fixed by saving the caller's pre-existing length.

- **`peek_to_end` / `peek_to_string` consume bytes on error**: both
  functions returned `Err` without mirroring partial data the reader had
  already consumed into the peek buffer. For `peek_to_string`, this was
  especially bad because `Read::read_to_string` rolls the String back on
  UTF-8 failure, making consumed-byte detection impossible. The sync
  `peek_to_string` now uses `read_to_end` into a raw `Vec<u8>` and
  mirrors unconditionally before validating UTF-8.

- **`peek_to_string` error-path semantics**: on any error (I/O or
  `InvalidData`) the caller's `buf` is left unchanged, matching
  `std::io::Read::read_to_string`'s contract. Consumed bytes are
  preserved in the internal peek buffer and accessible via `get_ref()`.

- **`peek_to_end` error-path semantics**: on error, partial data
  stays in `buf`, matching `std::io::Read::read_to_end`'s contract.
  Consumed bytes are also mirrored into the internal peek buffer.

#### Changed

- **Staging buffer for async `peek_to_end` / `peek_to_string`**: the
  per-poll stack-local `[u8; 8192]` array was replaced with a
  `StagingBuf` field stored in each future struct. When the `smallvec`
  feature is enabled (default), this is `SmallVec<[u8; 1024]>` — inline
  in the future struct with automatic heap spill. Without `smallvec`, a
  minimal fixed-array wrapper provides the same inline semantics. This
  eliminates stack pressure inside `poll()` for executors with small
  per-task stacks.

## RELEASED

### 0.5.0

#### Fixed (correctness)

- **`tokio::AsyncPeekable::poll_peek`**: removed the duplicate `put_slice`
  on the `Pending` branch. Previously, when the inner reader returned
  `Pending` while topping up the peek buffer, the buffered bytes were
  written into the caller's `ReadBuf` twice (e.g. `[1, 2, 3]` became
  `[1, 2, 3, 1, 2, 3]`).
- **`tokio::AsyncPeekable::poll_read`**: on the `Pending` branch the
  buffered bytes were left in `this.buffer` and re-emitted on the next
  successful read, causing duplicated data downstream. Now consumes the
  peek buffer and returns a partial `Ready(Ok(()))` (a valid
  `AsyncRead` outcome).
- **`tokio::AsyncPeekable::poll_read` / `poll_peek`** error paths: the
  `unsafe { buf.inner_mut() }.fill(MaybeUninit::uninit())` rollback was
  off by the partial-read length and didn't actually adjust
  `buf.filled()`. Replaced with a proper `set_filled(orig_filled)`
  rollback so the caller's `ReadBuf` is untouched on error.
- **`Peekable::peek` / `Peekable::peek_exact`** now retry on
  `ErrorKind::Interrupted` per the documented contract. Previously the
  error was propagated, breaking on Unix signals.
- **`Peekable::peek` / `Peekable::fill_peek_buf`**: when an inner read
  errored after a `resize`, the peek buffer was left at the temporarily
  resized length holding zero-bytes. Subsequent peeks would return
  those zeros as if they were real peeked data. Now the buffer is
  truncated back to its original length on error.
- **`future::AsyncPeekable::poll_peek`**: same `resize` rollback fix on
  the `Err` and `Pending` branches.
- **`{tokio,future}::FillPeekBuf`**: the underlying
  `me.peeker.buffer.resize(cap)?` was never rolled back on `Err` or
  `Pending`, leaving ghost zero-bytes in the peek buffer. Fixed.

#### Documented behaviour clarifications

- `Peekable::done` semantics on a zero counter remain unchanged
  (silent no-op returning the unchanged count), now explicitly
  documented.

#### Test coverage

- New integration test files `tests/sync.rs`, `tests/tokio_io.rs`,
  `tests/futures_io.rs`. Each one ships a deterministic flaky reader
  (returns `Pending` / `Interrupted` / arbitrary `Err` on cue) so the
  bug-fix paths above all have regression tests.
- Coverage rose from **70.7% to ~95%** (676/712 lines), and all 16
  source files are at 90%+ individually.

#### CI / metadata

- Bumped the declared MSRV from `1.56` (which never actually built —
  `dep:` feature prefix needs `1.60+`, and the current `tokio` floor is
  `1.71`) to `1.71`. Added an `msrv` job that dynamically reads
  `rust-version` from `Cargo.toml` so future drift is caught
  automatically.
- CI now uses `taiki-e/install-action` for `cargo-hack` and
  `cargo-tarpaulin` (binary install instead of source compile).
- Clippy step now runs with `--all-targets` so test code is also
  linted.

### 0.2.4 (Dec 20st, 2024)

- bumpup `futures-util` version

### 0.2.3 (Mar 21st, 2024)

- Add `consume_in_place`, `peekable_with_capacity` and `fill_peek_buf` APIs

### 0.2.2 (Mar 21st, 2024)

- Add `consume` API
- Add DocTests for some APIs

### 0.2.1 (Feb 10th, 2024)

- Fix `peek_exact` bug and add `peek_exact_peek_exact_read_exact` test cases

### 0.2.0 (Jan 30th, 2024)

- Add `get_ref`, `get_mut` and `into_components` APIs

### 0.1.0 (2024)

- `AsyncPeekable` and `Peekable`
