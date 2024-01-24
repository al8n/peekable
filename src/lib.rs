//! A template for creating Rust open-source repo on GitHub
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

/// template
pub fn it_works() -> usize {
  4
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_works() {
    assert_eq!(it_works(), 4);
  }
}
