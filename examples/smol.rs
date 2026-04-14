//! Smol example: detect the protocol of an incoming async stream by
//! peeking at the first few bytes (without consuming them), then read
//! the rest of the stream as if peek never happened.
//!
//! Smol uses the `futures_util::io::AsyncRead` trait, so we work
//! through `peekable::future::AsyncPeekable`. The same example will
//! also work with `async-std` and any other runtime built on
//! `futures::io`.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example smol --features future
//! ```

use futures::io::{AsyncReadExt, Cursor};

use peekable::future::AsyncPeekExt;

fn main() -> std::io::Result<()> {
  smol::block_on(async {
    // `futures::io::Cursor` stands in for any
    // `futures_util::io::AsyncRead` (`smol::net::TcpStream`,
    // `async_std::net::TcpStream`, ...).
    let stream = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec());
    let mut peekable = stream.peekable();

    // Peek the first 4 bytes — no data is consumed from the stream.
    let mut magic = [0u8; 4];
    peekable.peek_exact(&mut magic).await?;

    let kind = match &magic {
      b"HTTP" => "HTTP",
      b"SSH-" => "SSH",
      // TLS handshake starts with 0x16 0x03 0x0{1,3} ...
      [0x16, 0x03, 0x01..=0x03, _] => "TLS",
      _ => "unknown",
    };
    println!("[smol] detected protocol: {kind}");

    // Read everything — the bytes we peeked are still here.
    let mut full = String::new();
    peekable.read_to_string(&mut full).await?;
    assert!(
      full.starts_with("HTTP"),
      "peeked bytes should still be readable"
    );
    println!("[smol] full payload ({} bytes):\n{}", full.len(), full);

    Ok(())
  })
}
