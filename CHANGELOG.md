# CHANGELOG

## UNRELEASED

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
