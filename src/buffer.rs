use std::ops::RangeTo;

/// A trait for types that can be used as a buffer in the `Peekable` and `AsyncPeekable` reader.
pub trait Buffer {
  /// Create a new empty buffer.
  fn new() -> Self
  where
    Self: Sized;

  /// Create a new buffer with the specified capacity.
  fn with_capacity(capacity: usize) -> Self
  where
    Self: Sized;

  /// Consume the range from the buffer, the remaining elements are shifted to the left.
  fn consume(&mut self, rng: RangeTo<usize>);

  /// Remove all elements from the buffer.
  fn clear(&mut self);

  /// Resizes the buffer so that its length is equal to len.
  fn resize(&mut self, len: usize) -> std::io::Result<()>;

  /// Shorten the buffer, keeping the first len elements and dropping the rest.
  fn truncate(&mut self, len: usize);

  /// Copy elements from a slice and append them to the buffer.
  fn extend_from_slice(&mut self, other: &[u8]) -> std::io::Result<()>;

  /// Returns a slice of the buffer.
  fn as_slice(&self) -> &[u8];

  /// Returns a mutable slice of the buffer.
  fn as_mut_slice(&mut self) -> &mut [u8];

  /// Returns the length of the buffer.
  fn len(&self) -> usize;

  /// Returns `true` if the buffer is empty.
  fn is_empty(&self) -> bool;

  /// Returns the capacity of the buffer.
  fn capacity(&self) -> usize;
}

impl Buffer for Vec<u8> {
  fn new() -> Self {
    Vec::new()
  }

  fn with_capacity(capacity: usize) -> Self {
    Vec::with_capacity(capacity)
  }

  fn consume(&mut self, rng: RangeTo<usize>) {
    self.drain(rng);
  }

  fn clear(&mut self) {
    self.clear();
  }

  fn resize(&mut self, len: usize) -> std::io::Result<()> {
    self.resize(len, 0);
    Ok(())
  }

  fn truncate(&mut self, len: usize) {
    self.truncate(len);
  }

  fn extend_from_slice(&mut self, other: &[u8]) -> std::io::Result<()> {
    self.extend_from_slice(other);
    Ok(())
  }

  fn as_slice(&self) -> &[u8] {
    self.as_slice()
  }

  fn as_mut_slice(&mut self) -> &mut [u8] {
    self.as_mut_slice()
  }

  fn len(&self) -> usize {
    self.len()
  }

  fn is_empty(&self) -> bool {
    self.is_empty()
  }

  fn capacity(&self) -> usize {
    self.capacity()
  }
}

#[cfg(feature = "smallvec")]
impl<A> Buffer for smallvec::SmallVec<A>
where
  A: smallvec::Array<Item = u8>,
{
  fn new() -> Self
  where
    Self: Sized,
  {
    smallvec::SmallVec::new()
  }

  fn with_capacity(capacity: usize) -> Self
  where
    Self: Sized,
  {
    smallvec::SmallVec::with_capacity(capacity)
  }

  fn consume(&mut self, rng: RangeTo<usize>) {
    self.drain(rng);
  }

  fn clear(&mut self) {
    self.clear();
  }

  fn resize(&mut self, len: usize) -> std::io::Result<()> {
    self.resize(len, 0);
    Ok(())
  }

  fn truncate(&mut self, len: usize) {
    self.truncate(len);
  }

  fn extend_from_slice(&mut self, other: &[u8]) -> std::io::Result<()> {
    self.extend_from_slice(other);
    Ok(())
  }

  fn as_slice(&self) -> &[u8] {
    self.as_slice()
  }

  fn as_mut_slice(&mut self) -> &mut [u8] {
    self.as_mut_slice()
  }

  fn len(&self) -> usize {
    self.len()
  }

  fn is_empty(&self) -> bool {
    self.is_empty()
  }

  fn capacity(&self) -> usize {
    self.capacity()
  }
}

#[cfg(feature = "tinyvec")]
impl<A: tinyvec::Array<Item = u8>> Buffer for tinyvec::TinyVec<A> {
  fn new() -> Self
  where
    Self: Sized,
  {
    tinyvec::TinyVec::new()
  }

  fn with_capacity(capacity: usize) -> Self
  where
    Self: Sized,
  {
    tinyvec::TinyVec::with_capacity(capacity)
  }

  fn consume(&mut self, rng: RangeTo<usize>) {
    self.drain(rng);
  }

  fn clear(&mut self) {
    self.clear();
  }

  fn resize(&mut self, len: usize) -> std::io::Result<()> {
    self.resize(len, 0);
    Ok(())
  }

  fn truncate(&mut self, len: usize) {
    self.truncate(len);
  }

  fn extend_from_slice(&mut self, other: &[u8]) -> std::io::Result<()> {
    self.extend_from_slice(other);
    Ok(())
  }

  fn as_slice(&self) -> &[u8] {
    self.as_slice()
  }

  fn as_mut_slice(&mut self) -> &mut [u8] {
    self.as_mut_slice()
  }

  fn len(&self) -> usize {
    self.len()
  }

  fn is_empty(&self) -> bool {
    self.is_empty()
  }

  fn capacity(&self) -> usize {
    self.capacity()
  }
}
