//! Bounded media input for the voice bridge. No devices, models or file access.
use anyhow::{Result, anyhow, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};

const MAX_PCM_BYTES: usize = 640_000;
const MAX_WAV_BYTES: usize = 1024 * 1024;
const MAX_WAV_BASE64_CHARS: usize = MAX_WAV_BYTES.div_ceil(3) * 4;
const MAX_SPEECH_UTF16_UNITS: usize = 2000;
const MAX_SEGMENT_CHARACTERS: usize = 320;

/// Accepts a complete RIFF/WAVE container containing one PCM16LE mono 16 kHz
/// stream. Ancillary chunks are ignored; their lengths and padding remain checked.
pub(crate) fn decode_wav_base64(encoded: &str) -> Result<Vec<u8>> {
    ensure!(
        !encoded.is_empty() && encoded.len() <= MAX_WAV_BASE64_CHARS,
        "voice_wav_base64_limit"
    );
    let wav = STANDARD
        .decode(encoded)
        .map_err(|_| anyhow!("voice_wav_base64_invalid"))?;
    ensure!(
        (12..=MAX_WAV_BYTES).contains(&wav.len()),
        "voice_wav_size_invalid"
    );
    ensure!(
        &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE",
        "voice_wav_container_invalid"
    );
    let riff_length = u32::from_le_bytes(wav[4..8].try_into()?) as usize;
    ensure!(
        riff_length.checked_add(8) == Some(wav.len()),
        "voice_wav_riff_length_invalid"
    );
    let mut offset = 12usize;
    let mut format_seen = false;
    let mut pcm_range = None;
    while offset < wav.len() {
        ensure!(wav.len() - offset >= 8, "voice_wav_chunk_truncated");
        let chunk_length = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(chunk_length)
            .ok_or_else(|| anyhow!("voice_wav_chunk_truncated"))?;
        let padded_end = end
            .checked_add(chunk_length % 2)
            .ok_or_else(|| anyhow!("voice_wav_chunk_truncated"))?;
        ensure!(padded_end <= wav.len(), "voice_wav_chunk_truncated");
        match &wav[offset..offset + 4] {
            b"fmt " => {
                ensure!(!format_seen, "voice_wav_duplicate_format");
                format_seen = true;
                // PCM's canonical 16-byte form and WAVEFORMATEX with cbSize=0.
                // Float, compressed and extensible formats are deliberately rejected.
                ensure!(
                    chunk_length == 16 || (chunk_length == 18 && wav[start + 16..end] == [0, 0]),
                    "voice_wav_format_invalid"
                );
                let format = &wav[start..end];
                let u16_at = |at| u16::from_le_bytes([format[at], format[at + 1]]);
                let u32_at = |at| {
                    u32::from_le_bytes([format[at], format[at + 1], format[at + 2], format[at + 3]])
                };
                ensure!(
                    u16_at(0) == 1
                        && u16_at(2) == 1
                        && u32_at(4) == 16_000
                        && u32_at(8) == 32_000
                        && u16_at(12) == 2
                        && u16_at(14) == 16,
                    "voice_wav_requires_pcm16_mono_16000"
                );
            }
            b"data" => {
                ensure!(pcm_range.is_none(), "voice_wav_duplicate_data");
                ensure!(
                    chunk_length > 0 && chunk_length <= MAX_PCM_BYTES && chunk_length % 2 == 0,
                    "voice_wav_pcm_size_invalid"
                );
                pcm_range = Some(start..end);
            }
            _ => {}
        }
        offset = padded_end;
    }
    ensure!(format_seen, "voice_wav_format_missing");
    let range = pcm_range.ok_or_else(|| anyhow!("voice_wav_data_missing"))?;
    Ok(wav[range].to_vec())
}

/// Splits at the last punctuation/whitespace boundary within the segment limit,
/// or at a Unicode character boundary when no natural boundary exists.
/// Concatenating the pieces always reproduces the original text exactly.
/// A long whitespace run may yield a whitespace-only piece; playback may skip it.
pub(crate) fn split_speech(text: &str) -> Result<Vec<String>> {
    ensure!(
        text.encode_utf16().take(MAX_SPEECH_UTF16_UNITS + 1).count() <= MAX_SPEECH_UTF16_UNITS,
        "voice_speech_text_limit"
    );
    ensure!(!text.trim().is_empty(), "voice_speech_text_empty");
    let mut pieces = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = 0;
        let mut preferred = None;
        for (at, character) in remaining.char_indices().take(MAX_SEGMENT_CHARACTERS) {
            end = at + character.len_utf8();
            if character.is_whitespace()
                || matches!(
                    character,
                    '.' | ','
                        | '!'
                        | '?'
                        | ';'
                        | ':'
                        | '\u{2026}'
                        | '。'
                        | '，'
                        | '！'
                        | '？'
                        | '；'
                        | '：'
                        | '、'
                )
            {
                preferred = Some(end);
            }
        }
        let split = if end == remaining.len() {
            end
        } else {
            preferred.unwrap_or(end)
        };
        pieces.push(remaining[..split].to_owned());
        remaining = &remaining[split..];
    }
    Ok(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_format() -> Vec<u8> {
        let mut format = Vec::new();
        format.extend(1u16.to_le_bytes());
        format.extend(1u16.to_le_bytes());
        format.extend(16_000u32.to_le_bytes());
        format.extend(32_000u32.to_le_bytes());
        format.extend(2u16.to_le_bytes());
        format.extend(16u16.to_le_bytes());
        format
    }

    fn wave(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        for (name, data) in chunks {
            bytes.extend_from_slice(*name);
            bytes.extend((data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(data);
            if data.len() % 2 != 0 {
                bytes.push(0);
            }
        }
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
        decode_wav_base64(&STANDARD.encode(bytes))
    }

    #[test]
    fn metadata_padding_and_pcm_format_extension_preserve_audio() -> Result<()> {
        let mut format = pcm_format();
        format.extend([0, 0]);
        let pcm = [0, 0, 0xff, 0x7f, 0, 0x80];
        let bytes = wave(&[
            (b"JUNK", b"odd"),
            (b"fmt ", &format),
            (b"LIST", b"metadata"),
            (b"data", &pcm),
            (b"note", b"x"),
        ]);
        assert_eq!(decode(&bytes)?, pcm);
        Ok(())
    }

    #[test]
    fn malformed_containers_and_duplicate_required_chunks_are_rejected() {
        let format = pcm_format();
        let pcm = [1, 2];
        let valid = wave(&[(b"fmt ", &format), (b"data", &pcm)]);
        assert!(decode(&valid[..valid.len() - 1]).is_err());
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        let mut missing_pad = wave(&[(b"fmt ", &format), (b"data", &pcm), (b"JUNK", b"x")]);
        missing_pad.pop();
        let size = (missing_pad.len() - 8) as u32;
        missing_pad[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(decode(&missing_pad).is_err());
        for chunks in [
            vec![
                (b"fmt ", format.as_slice()),
                (b"fmt ", format.as_slice()),
                (b"data", &pcm),
            ],
            vec![
                (b"fmt ", format.as_slice()),
                (b"data", &pcm),
                (b"data", &pcm),
            ],
            vec![(b"data", pcm.as_slice())],
            vec![(b"fmt ", format.as_slice())],
        ] {
            assert!(decode(&wave(&chunks)).is_err());
        }
    }

    #[test]
    fn format_audio_and_encoded_size_limits_are_enforced() -> Result<()> {
        let format = pcm_format();
        for field in [0, 2, 4, 8, 12, 14] {
            let mut wrong = format.clone();
            wrong[field] ^= 1;
            assert!(decode(&wave(&[(b"fmt ", &wrong), (b"data", &[0, 0])])).is_err());
        }
        assert!(decode(&wave(&[(b"fmt ", &format), (b"data", &[])])).is_err());
        assert!(decode(&wave(&[(b"fmt ", &format), (b"data", &[0])])).is_err());
        let max = vec![0; MAX_PCM_BYTES];
        assert_eq!(
            decode(&wave(&[(b"fmt ", &format), (b"data", &max)]))?.len(),
            MAX_PCM_BYTES
        );
        assert!(
            decode(&wave(&[
                (b"fmt ", &format),
                (b"data", &vec![0; MAX_PCM_BYTES + 2])
            ]))
            .is_err()
        );
        assert!(decode_wav_base64(&"A".repeat(MAX_WAV_BASE64_CHARS + 1)).is_err());
        assert!(decode_wav_base64("!invalid!").is_err());
        Ok(())
    }

    #[test]
    fn speech_prefers_boundaries_and_preserves_exact_text() -> Result<()> {
        let text = format!("{}。 {}", "中".repeat(200), "a".repeat(450));
        let pieces = split_speech(&text)?;
        assert_eq!(pieces[0], format!("{}。 ", "中".repeat(200)));
        assert_eq!(pieces.concat(), text);
        assert!(
            pieces
                .iter()
                .all(|piece| piece.chars().count() <= MAX_SEGMENT_CHARACTERS)
        );
        let with_long_spaces = format!("start{}end", " ".repeat(700));
        assert_eq!(split_speech(&with_long_spaces)?.concat(), with_long_spaces);
        Ok(())
    }

    #[test]
    fn speech_uses_utf16_budget_and_never_splits_unicode_characters() -> Result<()> {
        let text = "😀".repeat(1000);
        let pieces = split_speech(&text)?;
        assert_eq!(pieces.concat(), text);
        assert!(
            pieces
                .iter()
                .all(|piece| piece.chars().count() <= MAX_SEGMENT_CHARACTERS)
        );
        assert!(split_speech(&format!("{text}a")).is_err());
        assert!(split_speech(&"a".repeat(2001)).is_err());
        assert!(split_speech("").is_err());
        assert!(split_speech(" \n\t\u{3000}").is_err());
        Ok(())
    }
}
