use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};

pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Streaming decoder for Chrome Native Messaging's u32-le JSON frames.
#[derive(Debug, Default)]
pub struct NativeDecoder {
    buffer: Vec<u8>,
}

impl NativeDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        self.buffer.extend_from_slice(bytes);
        let mut decoded = Vec::new();
        loop {
            if self.buffer.len() < 4 {
                break;
            }
            let length = u32::from_le_bytes(
                self.buffer[..4]
                    .try_into()
                    .expect("four-byte prefix already checked"),
            ) as usize;
            if length == 0 {
                bail!("zero-length native message");
            }
            if length > MAX_MESSAGE_BYTES {
                bail!("native message exceeds {MAX_MESSAGE_BYTES} bytes");
            }
            if self.buffer.len() < 4 + length {
                break;
            }
            let body = self.buffer[4..4 + length].to_vec();
            self.buffer.drain(..4 + length);
            let value = serde_json::from_slice(&body).context("invalid native-message JSON")?;
            decoded.push(value);
        }
        Ok(decoded)
    }

    pub fn finish(&self) -> Result<()> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            bail!("truncated native message")
        }
    }
}

/// Streaming decoder for Nova.app's newline-delimited bridge messages.
#[derive(Debug, Default)]
pub struct NdjsonDecoder {
    buffer: Vec<u8>,
}

impl NdjsonDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > MAX_MESSAGE_BYTES && !self.buffer.contains(&b'\n') {
            bail!("NDJSON bridge message exceeds {MAX_MESSAGE_BYTES} bytes");
        }
        let mut decoded = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            if newline > MAX_MESSAGE_BYTES {
                bail!("NDJSON bridge message exceeds {MAX_MESSAGE_BYTES} bytes");
            }
            let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                bail!("empty NDJSON bridge message");
            }
            let value = serde_json::from_slice(&line).context("invalid bridge JSON")?;
            decoded.push(value);
        }
        if self.buffer.len() > MAX_MESSAGE_BYTES {
            bail!("NDJSON bridge message exceeds {MAX_MESSAGE_BYTES} bytes");
        }
        Ok(decoded)
    }

    pub fn finish(&self) -> Result<()> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            bail!("truncated NDJSON bridge message")
        }
    }
}

pub fn encode_native(value: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value).context("serialize native message")?;
    check_length(body.len())?;
    let length = u32::try_from(body.len()).map_err(|_| anyhow!("native message is too large"))?;
    let mut encoded = Vec::with_capacity(body.len() + 4);
    encoded.extend_from_slice(&length.to_le_bytes());
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

pub fn encode_ndjson(value: &Value) -> Result<Vec<u8>> {
    let mut body = serde_json::to_vec(value).context("serialize bridge message")?;
    check_length(body.len())?;
    body.push(b'\n');
    Ok(body)
}

pub fn read_native<R: Read>(reader: &mut R) -> Result<Option<Value>> {
    let mut prefix = [0_u8; 4];
    let read = reader
        .read(&mut prefix)
        .context("read native-message prefix")?;
    if read == 0 {
        return Ok(None);
    }
    if read < prefix.len() {
        reader
            .read_exact(&mut prefix[read..])
            .context("truncated native-message prefix")?;
    }
    let length = u32::from_le_bytes(prefix) as usize;
    check_length(length)?;
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .context("truncated native-message body")?;
    serde_json::from_slice(&body)
        .context("invalid native-message JSON")
        .map(Some)
}

pub fn write_native<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    writer
        .write_all(&encode_native(value)?)
        .context("write native message")?;
    writer.flush().context("flush native message")
}

pub fn write_ndjson<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    writer
        .write_all(&encode_ndjson(value)?)
        .context("write bridge message")?;
    writer.flush().context("flush bridge message")
}

fn check_length(length: usize) -> Result<()> {
    if length == 0 {
        bail!("zero-length message");
    }
    if length > MAX_MESSAGE_BYTES {
        bail!("message exceeds {MAX_MESSAGE_BYTES} bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn native_decoder_handles_fragmented_and_concatenated_frames() {
        let first = encode_native(&json!({"kind": "first"})).unwrap();
        let second = encode_native(&json!({"kind": "second"})).unwrap();
        let split = first.len() - 2;
        let mut decoder = NativeDecoder::default();

        assert!(decoder.push(&first[..split]).unwrap().is_empty());
        let mut tail = first[split..].to_vec();
        tail.extend_from_slice(&second);
        assert_eq!(
            decoder.push(&tail).unwrap(),
            vec![json!({"kind": "first"}), json!({"kind": "second"})]
        );
        decoder.finish().unwrap();
    }

    #[test]
    fn native_decoder_rejects_malformed_zero_and_oversize_frames() {
        let mut malformed = (1_u32).to_le_bytes().to_vec();
        malformed.push(b'{');
        let error = NativeDecoder::default()
            .push(&malformed)
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid native-message JSON"), "{error}");

        let error = NativeDecoder::default()
            .push(&0_u32.to_le_bytes())
            .unwrap_err()
            .to_string();
        assert!(error.contains("zero-length"), "{error}");

        let oversize = u32::try_from(MAX_MESSAGE_BYTES + 1).unwrap().to_le_bytes();
        let error = NativeDecoder::default()
            .push(&oversize)
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceeds"), "{error}");
    }

    #[test]
    fn streaming_decoders_reject_truncated_input_at_eof() {
        let mut native = NativeDecoder::default();
        native.push(&[4, 0, 0, 0, b'{']).unwrap();
        assert!(native
            .finish()
            .unwrap_err()
            .to_string()
            .contains("truncated"));

        let mut ndjson = NdjsonDecoder::default();
        ndjson.push(br#"{"kind":"partial"}"#).unwrap();
        assert!(ndjson
            .finish()
            .unwrap_err()
            .to_string()
            .contains("truncated"));
    }

    #[test]
    fn ndjson_decoder_handles_crlf_and_rejects_bad_lines() {
        let mut decoder = NdjsonDecoder::default();
        assert_eq!(
            decoder.push(b"{\"n\":1}\r\n{\"n\":2}\n").unwrap(),
            vec![json!({"n": 1}), json!({"n": 2})]
        );
        decoder.finish().unwrap();

        let error = NdjsonDecoder::default()
            .push(b"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("empty NDJSON"), "{error}");

        let error = NdjsonDecoder::default()
            .push(b"not-json\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid bridge JSON"), "{error}");
    }

    #[test]
    fn ndjson_decoder_rejects_oversize_buffered_and_terminated_lines() {
        let oversize = vec![b'a'; MAX_MESSAGE_BYTES + 1];
        let error = NdjsonDecoder::default()
            .push(&oversize)
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceeds"), "{error}");

        let mut terminated = oversize;
        terminated.push(b'\n');
        let error = NdjsonDecoder::default()
            .push(&terminated)
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceeds"), "{error}");
    }

    #[test]
    fn blocking_native_reader_rejects_truncated_and_oversize_frames() {
        let mut truncated_prefix = Cursor::new(vec![1_u8, 0]);
        assert!(read_native(&mut truncated_prefix)
            .unwrap_err()
            .to_string()
            .contains("truncated native-message prefix"));

        let mut truncated_body = Cursor::new([3_u32.to_le_bytes().as_slice(), b"{}"].concat());
        assert!(read_native(&mut truncated_body)
            .unwrap_err()
            .to_string()
            .contains("truncated native-message body"));

        let mut oversize = Cursor::new(u32::try_from(MAX_MESSAGE_BYTES + 1).unwrap().to_le_bytes());
        assert!(read_native(&mut oversize)
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
    }
}
