//! Synchronous example: detect the protocol of an incoming stream by
//! peeking at the first few bytes (without consuming them), then read
//! the rest of the stream as if peek never happened.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example sync
//! ```

use std::io::{Cursor, Read};

use peekable::PeekExt;

fn main() -> std::io::Result<()> {
  // A `Cursor` stands in for any `std::io::Read` (`TcpStream`, file,
  // pipe, …).
  let stream = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec());
  let mut peekable = stream.peekable();

  // Peek the first 4 bytes — no data is consumed.
  let mut magic = [0u8; 4];
  peekable.peek_exact(&mut magic)?;

  let kind = match &magic {
    b"HTTP" => "HTTP",
    b"SSH-" => "SSH",
    // TLS handshake starts with 0x16 0x03 0x0{1,3} ...
    [0x16, 0x03, 0x01..=0x03, _] => "TLS",
    _ => "unknown",
  };
  println!("[sync] detected protocol: {kind}");

  // Read everything — the bytes we peeked are still here.
  let mut full = String::new();
  peekable.read_to_string(&mut full)?;
  assert!(
    full.starts_with("HTTP"),
    "peeked bytes should still be readable"
  );
  println!("[sync] full payload ({} bytes):\n{}", full.len(), full);

  Ok(())
}
