//! Bounded byte-only SSE framing for the isolated diagnostic.
//! JSON, UTF-8 validation and `[DONE]` policy belong to the caller.
use anyhow::{Result, bail, ensure};

const MAX_WIRE_BYTES: usize = 2 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 128 * 1024;
const MAX_DATA_BYTES: usize = 128 * 1024;

#[derive(Default)]
pub(super) struct SseDecoder {
    wire_bytes: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    data_lines: usize,
    nonempty_data: bool,
    frame_non_whitespace: bool,
    failed: bool,
}

impl SseDecoder {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        ensure!(!self.failed, "SSE decoder already failed");
        let result = self.push_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn push_inner(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        ensure!(
            bytes.len() <= MAX_WIRE_BYTES - self.wire_bytes,
            "SSE wire limit exceeded"
        );
        self.wire_bytes += bytes.len();
        let mut completed = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.complete_line(&line, &mut completed)?;
            } else {
                // Includes a possible CR before LF; no UTF-8 conversion here.
                ensure!(self.line.len() < MAX_LINE_BYTES, "SSE line limit exceeded");
                self.line.push(byte);
            }
        }
        Ok(completed)
    }

    fn complete_line(&mut self, line: &[u8], completed: &mut Vec<Vec<u8>>) -> Result<()> {
        if line.is_empty() {
            // Empty data events carry no JSON. Interior empty data lines still
            // contribute their standard newline when a frame has actual data.
            if self.nonempty_data {
                completed.push(std::mem::take(&mut self.data));
            } else {
                self.data.clear();
            }
            self.data_lines = 0;
            self.nonempty_data = false;
            self.frame_non_whitespace = false;
            return Ok(());
        }
        self.frame_non_whitespace |= line.iter().any(|b| !b.is_ascii_whitespace());
        if line[0] == b':' {
            return Ok(());
        }
        let (field, mut value) = match line.iter().position(|b| *b == b':') {
            Some(index) => (&line[..index], &line[index + 1..]),
            None => (line, &[][..]),
        };
        if field != b"data" {
            return Ok(());
        }
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        let separator = usize::from(self.data_lines != 0);
        ensure!(
            separator + value.len() <= MAX_DATA_BYTES - self.data.len(),
            "SSE data frame limit exceeded"
        );
        if separator != 0 {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(value);
        self.data_lines += 1;
        self.nonempty_data |= !value.is_empty();
        Ok(())
    }

    pub(super) fn finish(&mut self) -> Result<()> {
        ensure!(!self.failed, "SSE decoder already failed");
        if self.frame_non_whitespace || self.line.iter().any(|b| !b.is_ascii_whitespace()) {
            self.failed = true;
            bail!("SSE stream ended with an incomplete frame");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_unicode_crlf_and_multiple_data_lines_remain_exact_bytes() -> Result<()> {
        let wire =
            "data: {\"text\":\"停\",\r\ndata: \"n\":1}\r\n\r\ndata: [DONE]\r\n\r\n".as_bytes();
        // Every split includes cuts inside the UTF-8 character and between CR/LF.
        for cut in 0..=wire.len() {
            let mut decoder = SseDecoder::default();
            let mut frames = decoder.push(&wire[..cut])?;
            frames.extend(decoder.push(&wire[cut..])?);
            decoder.finish()?;
            assert_eq!(
                frames,
                vec![
                    "{\"text\":\"停\",\n\"n\":1}".as_bytes().to_vec(),
                    b"[DONE]".to_vec()
                ]
            );
        }
        Ok(())
    }

    #[test]
    fn comments_fields_and_empty_events_do_not_emit_payloads() -> Result<()> {
        let mut decoder = SseDecoder::default();
        let frames = decoder.push(b": comment\nid: 7\nevent: message\nretry: 50\n\ndata:\ndata\n\ndata: one\ndata:\ndata: two\n\n")?;
        assert_eq!(frames, vec![b"one\n\ntwo".to_vec()]);
        decoder.finish()?;
        Ok(())
    }

    #[test]
    fn truncated_line_or_frame_fails_and_error_is_sticky() -> Result<()> {
        for wire in [
            &b"data: {\"x\":1}"[..],
            &b"data: {\"x\":1}\n"[..],
            &b": incomplete comment"[..],
        ] {
            let mut decoder = SseDecoder::default();
            assert!(decoder.push(wire)?.is_empty());
            assert!(decoder.finish().is_err());
            assert!(decoder.failed);
            assert!(decoder.push(b"\n\ndata: {}\n\n").is_err());
            assert!(decoder.finish().is_err());
        }
        let mut whitespace = SseDecoder::default();
        assert!(whitespace.push(b" \t\r")?.is_empty());
        whitespace.finish()?;
        Ok(())
    }

    #[test]
    fn line_limit_is_bounded_even_for_ignored_fields() -> Result<()> {
        let mut decoder = SseDecoder::default();
        let mut line = vec![b'x'; MAX_LINE_BYTES];
        line[0] = b':';
        assert!(decoder.push(&line)?.is_empty());
        assert!(decoder.push(b"x").is_err());
        assert!(decoder.push(b"\n\n").is_err());
        assert!(decoder.finish().is_err());
        Ok(())
    }

    #[test]
    fn joined_frame_limit_includes_inserted_newline() -> Result<()> {
        let mut first = b"data:".to_vec();
        first.extend(std::iter::repeat_n(b'x', MAX_DATA_BYTES - 6));
        first.push(b'\n');
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(&first)?.is_empty());
        assert!(decoder.push(b"data:12345\n")?.is_empty());
        let frames = decoder.push(b"\n")?;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), MAX_DATA_BYTES);
        decoder.finish()?;

        let mut overflow = SseDecoder::default();
        overflow.push(&first)?;
        assert!(overflow.push(b"data:123456\n").is_err());
        assert!(overflow.failed);
        assert!(overflow.push(b"\n").is_err());
        Ok(())
    }

    #[test]
    fn wire_limit_counts_ignored_bytes_across_pushes() -> Result<()> {
        let mut decoder = SseDecoder::default();
        let comment = b": ignored\n\n";
        let count = MAX_WIRE_BYTES / comment.len();
        for _ in 0..count {
            assert!(decoder.push(comment)?.is_empty());
        }
        let remainder = MAX_WIRE_BYTES % comment.len();
        assert!(decoder.push(&vec![b' '; remainder])?.is_empty());
        assert_eq!(decoder.wire_bytes, MAX_WIRE_BYTES);
        decoder.finish()?;
        assert!(decoder.push(b" ").is_err());
        assert!(decoder.failed);
        assert!(decoder.push(b"").is_err());
        Ok(())
    }
}
