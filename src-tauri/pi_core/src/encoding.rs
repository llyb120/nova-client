//! Text encoding detection and decoding, ported from `alkaid-core.mjs`.
//! Handles UTF-8 / UTF-16LE / UTF-16BE with BOM, plus a NUL-alternation
//! heuristic for BOM-less UTF-16.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Port of `detectTextEncoding(buffer)`: returns the encoding and the number of
/// BOM bytes to skip.
pub fn detect_text_encoding(buffer: &[u8]) -> (Encoding, usize) {
    if buffer.len() >= 2 && buffer[0] == 0xff && buffer[1] == 0xfe {
        return (Encoding::Utf16Le, 2);
    }
    if buffer.len() >= 2 && buffer[0] == 0xfe && buffer[1] == 0xff {
        return (Encoding::Utf16Be, 2);
    }
    if buffer.len() >= 3 && buffer[0] == 0xef && buffer[1] == 0xbb && buffer[2] == 0xbf {
        return (Encoding::Utf8, 3);
    }

    let sample_length = buffer.len().min(512);
    let mut even_nuls = 0usize;
    let mut odd_nuls = 0usize;
    for (i, byte) in buffer.iter().take(sample_length).enumerate() {
        if *byte != 0 {
            continue;
        }
        if i % 2 == 0 {
            even_nuls += 1;
        } else {
            odd_nuls += 1;
        }
    }
    let pairs = sample_length / 2;
    if pairs >= 4 {
        let pairs_f = pairs as f64;
        if odd_nuls as f64 / pairs_f > 0.6 && even_nuls as f64 / pairs_f < 0.1 {
            return (Encoding::Utf16Le, 0);
        }
        if even_nuls as f64 / pairs_f > 0.6 && odd_nuls as f64 / pairs_f < 0.1 {
            return (Encoding::Utf16Be, 0);
        }
    }
    (Encoding::Utf8, 0)
}

/// Port of `swapUtf16Bytes(buffer)`: swap each adjacent byte pair in place.
pub fn swap_utf16_bytes(buffer: &[u8]) -> Vec<u8> {
    let mut out = buffer.to_vec();
    let mut i = 0usize;
    while i + 1 < out.len() {
        out.swap(i, i + 1);
        i += 2;
    }
    out
}

fn decode_utf16le_bytes(buffer: &[u8]) -> String {
    let units: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Port of `decodeTextBuffer(buffer)`: detect encoding, strip the BOM, and
/// decode. Invalid UTF-8 and lone surrogates decode to U+FFFD, matching Node's
/// lossy `toString` as observed through the JSON test boundary.
pub fn decode_text_buffer(buffer: &[u8]) -> String {
    let (encoding, bom_bytes) = detect_text_encoding(buffer);
    let content = &buffer[bom_bytes..];
    match encoding {
        Encoding::Utf16Be => decode_utf16le_bytes(&swap_utf16_bytes(content)),
        Encoding::Utf16Le => decode_utf16le_bytes(content),
        Encoding::Utf8 => String::from_utf8_lossy(content).into_owned(),
    }
}
