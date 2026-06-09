//! Length-prefixed JSON-RPC framing.
//!
//! LSP wraps every message in an HTTP-style header:
//!
//! ```text
//! Content-Length: 123\r\n
//! \r\n
//! {"jsonrpc":"2.0", ...}
//! ```
//!
//! Other Content-* headers are tolerated and skipped (some servers emit
//! `Content-Type`). The decoder yields one body per frame; encoding wraps
//! a body in the required header.

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::LspError;

#[derive(Debug, Default)]
pub struct LspCodec;

impl Decoder for LspCodec {
    type Item = Vec<u8>;
    type Error = LspError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let Some(header_end) = find_subslice(src, b"\r\n\r\n") else {
            return Ok(None);
        };
        let header_bytes = &src[..header_end];
        let header_str = std::str::from_utf8(header_bytes).map_err(|e| LspError::Frame {
            reason: format!("non-utf8 header: {e}"),
        })?;
        let mut content_length: Option<usize> = None;
        for line in header_str.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            let (name, value) = line.split_once(':').ok_or_else(|| LspError::Frame {
                reason: format!("malformed header line: {line:?}"),
            })?;
            if name.eq_ignore_ascii_case("content-length") {
                let n: usize = value.trim().parse().map_err(|e| LspError::Frame {
                    reason: format!("bad content-length {value:?}: {e}"),
                })?;
                content_length = Some(n);
            }
        }
        let body_len = content_length.ok_or_else(|| LspError::Frame {
            reason: "missing Content-Length header".to_string(),
        })?;
        let total = header_end + 4 + body_len;
        if src.len() < total {
            // Reserve so the next read doesn't keep growing.
            src.reserve(total - src.len());
            return Ok(None);
        }
        let body = src[header_end + 4..total].to_vec();
        src.advance(total);
        Ok(Some(body))
    }
}

impl Encoder<Vec<u8>> for LspCodec {
    type Error = LspError;

    fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let header = format!("Content-Length: {}\r\n\r\n", item.len());
        dst.reserve(header.len() + item.len());
        dst.put_slice(header.as_bytes());
        dst.put_slice(&item);
        Ok(())
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn round_trip_single_frame() {
        let mut codec = LspCodec;
        let mut buf = BytesMut::new();
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#.to_vec();
        codec.encode(body.clone(), &mut buf).unwrap();
        let decoded = codec
            .decode(&mut buf)
            .unwrap()
            .expect("frame should decode");
        assert_eq!(decoded, body);
        assert!(buf.is_empty());
    }

    #[test]
    fn handles_split_frames() {
        let mut codec = LspCodec;
        let mut buf = BytesMut::new();
        let body = br#"{"a":1}"#;
        buf.extend_from_slice(b"Content-Length: 7\r\n\r\n");
        // Body arrives in two reads.
        assert!(codec.decode(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&body[..3]);
        assert!(codec.decode(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&body[3..]);
        let frame = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&frame, body);
    }

    #[test]
    fn tolerates_content_type_header() {
        let mut codec = LspCodec;
        let mut buf = BytesMut::new();
        buf.extend_from_slice(
            b"Content-Length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}",
        );
        let frame = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&frame, b"{}");
    }
}
