//! `Content-Encoding` handling without external compression crates.
//!
//! The client advertises the codings it can decode and decodes them in-process
//! with a small, bounds-checked DEFLATE implementation (gzip and deflate).
//! Brotli and unknown codings cannot be decoded here; they are rejected with a
//! typed [`HttpError`] instead of silently handing compressed bytes to callers.

use crate::client::HttpError;

/// A single `Content-Encoding` coding we know how to reason about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentEncoding {
    /// No transformation (`identity` or an empty token).
    Identity,
    /// gzip (`gzip`, `x-gzip`).
    Gzip,
    /// zlib-wrapped or raw DEFLATE.
    Deflate,
    /// Brotli (recognised but not decodable without a crate).
    Brotli,
    /// Any other coding, preserved for diagnostics.
    Unknown(String),
}

impl ContentEncoding {
    /// Parses one coding token, case-insensitively.
    pub fn parse(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "" | "identity" => Self::Identity,
            "gzip" | "x-gzip" => Self::Gzip,
            "deflate" => Self::Deflate,
            "br" => Self::Brotli,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// Decodes a response body according to its `Content-Encoding` header.
///
/// Codings are applied in the order listed, so they must be undone in reverse.
/// `limit` bounds the decompressed size to avoid decompression bombs. A missing
/// header (or `identity`) leaves the body unchanged.
pub fn decode_body(header: Option<&str>, body: &[u8], limit: usize) -> Result<Vec<u8>, HttpError> {
    let Some(header) = header else {
        return Ok(body.to_vec());
    };
    let codings: Vec<ContentEncoding> = header
        .split(',')
        .map(ContentEncoding::parse)
        .filter(|coding| !matches!(coding, ContentEncoding::Identity))
        .collect();
    if codings.is_empty() {
        return Ok(body.to_vec());
    }

    let mut data = body.to_vec();
    for coding in codings.into_iter().rev() {
        data = match coding {
            ContentEncoding::Identity => data,
            ContentEncoding::Gzip => inflate(&data, Wrapper::Gzip, limit)?,
            ContentEncoding::Deflate => inflate(&data, Wrapper::MaybeZlib, limit)?,
            ContentEncoding::Brotli => {
                return Err(HttpError::UnsupportedContentEncoding("br".into()));
            }
            ContentEncoding::Unknown(name) => {
                return Err(HttpError::UnsupportedContentEncoding(name));
            }
        };
    }
    Ok(data)
}

fn decompress_error(message: &str) -> HttpError {
    HttpError::Decompression(message.to_string())
}

#[derive(Clone, Copy)]
enum Wrapper {
    Gzip,
    MaybeZlib,
}

/// Reads bits LSB-first, as required by the DEFLATE bit stream.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, HttpError> {
        let mut value = 0_u32;
        for index in 0..count {
            let byte = *self
                .data
                .get(self.bit / 8)
                .ok_or_else(|| decompress_error("truncated deflate stream"))?;
            value |= u32::from((byte >> (self.bit % 8)) & 1) << index;
            self.bit += 1;
        }
        Ok(value)
    }

    fn align_to_byte(&mut self) {
        self.bit = (self.bit + 7) & !7;
    }

    fn read_bytes(&mut self, count: usize) -> Result<&'a [u8], HttpError> {
        self.align_to_byte();
        let start = self.bit / 8;
        let end = start
            .checked_add(count)
            .ok_or_else(|| decompress_error("deflate length overflow"))?;
        let slice = self
            .data
            .get(start..end)
            .ok_or_else(|| decompress_error("truncated deflate stream"))?;
        self.bit = end * 8;
        Ok(slice)
    }
}

const INVALID_SYMBOL: u16 = u16::MAX;
const MAX_BITS: u8 = 15;
const TABLE_SIZE: usize = 1 << MAX_BITS;

/// A canonical Huffman decoder represented by a fixed 2^15 lookup table.
struct Huffman {
    table: Vec<(u16, u8)>,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> Result<Self, HttpError> {
        let mut counts = [0_u32; MAX_BITS as usize + 1];
        for &length in lengths {
            if length > MAX_BITS {
                return Err(decompress_error("invalid huffman code length"));
            }
            if length > 0 {
                counts[length as usize] += 1;
            }
        }

        let mut next_code = [0_u32; MAX_BITS as usize + 1];
        let mut code = 0_u32;
        for bits in 1..=MAX_BITS as usize {
            code = (code + counts[bits - 1]) << 1;
            next_code[bits] = code;
        }

        let mut table = vec![(INVALID_SYMBOL, 0); TABLE_SIZE];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length == 0 {
                continue;
            }
            let canonical = next_code[length as usize];
            next_code[length as usize] += 1;
            let reversed = reverse_bits(canonical, length);
            let step = 1_usize << length;
            let mut index = reversed as usize;
            while index < TABLE_SIZE {
                table[index] = (symbol as u16, length);
                index += step;
            }
        }
        Ok(Self { table })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, HttpError> {
        let mut code = 0_usize;
        for width in 1..=MAX_BITS as usize {
            code |= (reader.read_bits(1)? as usize) << (width - 1);
            let (symbol, length) = self.table[code];
            // A shorter code fills many table slots; accept only once exactly
            // its own number of bits has been consumed.
            if symbol != INVALID_SYMBOL && usize::from(length) == width {
                return Ok(symbol);
            }
        }
        Err(decompress_error("invalid huffman code"))
    }
}

fn reverse_bits(mut code: u32, length: u8) -> u32 {
    let mut reversed = 0_u32;
    for _ in 0..length {
        reversed = (reversed << 1) | (code & 1);
        code >>= 1;
    }
    reversed
}

const LENGTH_BASE: [usize; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [usize; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn fixed_trees() -> (Huffman, Huffman) {
    let mut lengths = [0_u8; 288];
    lengths[..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..].fill(8);
    let literal = Huffman::from_lengths(&lengths).expect("fixed literal tree is valid");
    let distance = Huffman::from_lengths(&[5_u8; 32]).expect("fixed distance tree is valid");
    (literal, distance)
}

fn dynamic_trees(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman), HttpError> {
    let literal_count = reader.read_bits(5)? as usize + 257;
    let distance_count = reader.read_bits(5)? as usize + 1;
    let code_length_count = reader.read_bits(4)? as usize + 4;

    let mut code_lengths = [0_u8; 19];
    for &position in CODE_LENGTH_ORDER.iter().take(code_length_count) {
        code_lengths[position] = reader.read_bits(3)? as u8;
    }
    let code_tree = Huffman::from_lengths(&code_lengths)?;

    let total = literal_count + distance_count;
    let mut lengths = vec![0_u8; total];
    let mut index = 0;
    while index < total {
        match code_tree.decode(reader)? {
            symbol @ 0..=15 => {
                lengths[index] = symbol as u8;
                index += 1;
            }
            16 => {
                if index == 0 {
                    return Err(decompress_error("repeat with no previous code length"));
                }
                let previous = lengths[index - 1];
                let repeat = reader.read_bits(2)? as usize + 3;
                for _ in 0..repeat {
                    if index >= total {
                        return Err(decompress_error("code length repeat overflow"));
                    }
                    lengths[index] = previous;
                    index += 1;
                }
            }
            symbol @ (17 | 18) => {
                let repeat = if symbol == 17 {
                    reader.read_bits(3)? as usize + 3
                } else {
                    reader.read_bits(7)? as usize + 11
                };
                for _ in 0..repeat {
                    if index >= total {
                        return Err(decompress_error("code length repeat overflow"));
                    }
                    lengths[index] = 0;
                    index += 1;
                }
            }
            _ => return Err(decompress_error("invalid code length symbol")),
        }
    }

    let literal = Huffman::from_lengths(&lengths[..literal_count])?;
    let distance = Huffman::from_lengths(&lengths[literal_count..])?;
    Ok((literal, distance))
}

fn decode_block(
    reader: &mut BitReader<'_>,
    literal: &Huffman,
    distance: &Huffman,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<(), HttpError> {
    loop {
        match literal.decode(reader)? {
            symbol @ 0..=255 => {
                push_byte(out, symbol as u8, limit)?;
            }
            256 => return Ok(()),
            symbol @ 257..=285 => {
                let index = (symbol - 257) as usize;
                let length = LENGTH_BASE[index] + reader.read_bits(LENGTH_EXTRA[index])? as usize;
                let distance_symbol = distance.decode(reader)?;
                let distance_index = distance_symbol as usize;
                if distance_index >= DIST_BASE.len() {
                    return Err(decompress_error("invalid distance code"));
                }
                let back = DIST_BASE[distance_index]
                    + reader.read_bits(DIST_EXTRA[distance_index])? as usize;
                if back == 0 || back > out.len() {
                    return Err(decompress_error("distance exceeds decoded output"));
                }
                for _ in 0..length {
                    let byte = out[out.len() - back];
                    push_byte(out, byte, limit)?;
                }
            }
            _ => return Err(decompress_error("reserved length symbol")),
        }
    }
}

fn push_byte(out: &mut Vec<u8>, byte: u8, limit: usize) -> Result<(), HttpError> {
    if out.len() >= limit {
        return Err(HttpError::PayloadTooLarge);
    }
    out.push(byte);
    Ok(())
}

fn read_u16(bytes: &[u8], position: usize) -> Result<u16, HttpError> {
    let slice = bytes
        .get(position..position + 2)
        .ok_or_else(|| decompress_error("truncated deflate header"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn skip_zero_terminated(bytes: &[u8], mut position: usize) -> Result<usize, HttpError> {
    loop {
        let byte = *bytes
            .get(position)
            .ok_or_else(|| decompress_error("truncated gzip header"))?;
        position += 1;
        if byte == 0 {
            return Ok(position);
        }
    }
}

fn gzip_payload(bytes: &[u8]) -> Result<&[u8], HttpError> {
    if bytes.len() < 10 || bytes[..3] != [0x1f, 0x8b, 0x08] {
        return Err(decompress_error("invalid gzip header"));
    }
    let flags = bytes[3];
    let mut position = 10;
    if flags & 0x04 != 0 {
        let extra = usize::from(read_u16(bytes, position)?);
        position = position
            .checked_add(2 + extra)
            .ok_or_else(|| decompress_error("invalid gzip extra field"))?;
    }
    if flags & 0x08 != 0 {
        position = skip_zero_terminated(bytes, position)?;
    }
    if flags & 0x10 != 0 {
        position = skip_zero_terminated(bytes, position)?;
    }
    if flags & 0x02 != 0 {
        position = position
            .checked_add(2)
            .ok_or_else(|| decompress_error("invalid gzip header crc"))?;
    }
    bytes
        .get(position..)
        .ok_or_else(|| decompress_error("truncated gzip header"))
}

fn deflate_payload(bytes: &[u8]) -> &[u8] {
    // HTTP's `deflate` is raw DEFLATE, but some servers send the zlib wrapper.
    // A valid zlib header has CM=8 and a checksum divisible by 31.
    if bytes.len() >= 2 && bytes[0] & 0x0f == 8 {
        let header = (u16::from(bytes[0]) << 8) | u16::from(bytes[1]);
        if header % 31 == 0 {
            return &bytes[2..];
        }
    }
    bytes
}

fn inflate(input: &[u8], wrapper: Wrapper, limit: usize) -> Result<Vec<u8>, HttpError> {
    let payload = match wrapper {
        Wrapper::Gzip => gzip_payload(input)?,
        Wrapper::MaybeZlib => deflate_payload(input),
    };
    let mut reader = BitReader::new(payload);
    let mut out = Vec::new();
    loop {
        let final_block = reader.read_bits(1)? != 0;
        match reader.read_bits(2)? {
            0 => {
                let length = usize::from(read_bytes_as_u16(&mut reader)?);
                let inverse = usize::from(read_bytes_as_u16(&mut reader)?);
                if (length ^ 0xffff) != inverse {
                    return Err(decompress_error("stored block length mismatch"));
                }
                if out.len().saturating_add(length) > limit {
                    return Err(HttpError::PayloadTooLarge);
                }
                let bytes = reader.read_bytes(length)?;
                out.extend_from_slice(bytes);
            }
            1 => {
                let (literal, distance) = fixed_trees();
                decode_block(&mut reader, &literal, &distance, &mut out, limit)?;
            }
            2 => {
                let (literal, distance) = dynamic_trees(&mut reader)?;
                decode_block(&mut reader, &literal, &distance, &mut out, limit)?;
            }
            _ => return Err(decompress_error("reserved deflate block type")),
        }
        if final_block {
            return Ok(out);
        }
    }
}

fn read_bytes_as_u16(reader: &mut BitReader<'_>) -> Result<u16, HttpError> {
    let bytes = reader.read_bytes(2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &[u8] = b"Hello, caf\xc3\xa9! The quick brown fox jumps over the lazy dog. \
        Hello, caf\xc3\xa9! The quick brown fox jumps over the lazy dog. \
        Hello, caf\xc3\xa9! The quick brown fox jumps over the lazy dog. \
        Hello, caf\xc3\xa9! The quick brown fox jumps over the lazy dog. ";

    const RAW_DEFLATE: &[u8] = &[
        0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x48, 0x4e, 0x4c, 0x3b, 0xbc, 0x52, 0x51, 0x21,
        0x24, 0x23, 0x55, 0xa1, 0xb0, 0x34, 0x33, 0x39, 0x5b, 0x21, 0xa9, 0x28, 0xbf, 0x3c, 0x4f,
        0x21, 0x2d, 0xbf, 0x42, 0x21, 0xab, 0x34, 0xb7, 0xa0, 0x58, 0x21, 0xbf, 0x2c, 0xb5, 0x48,
        0xa1, 0x04, 0x28, 0x9d, 0x93, 0x58, 0x55, 0xa9, 0x90, 0x92, 0x9f, 0xae, 0xa7, 0xe0, 0x31,
        0xd4, 0xb4, 0x02, 0x00,
    ];
    const ZLIB_DEFLATE: &[u8] = &[
        0x78, 0xda, 0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x48, 0x4e, 0x4c, 0x3b, 0xbc, 0x52,
        0x51, 0x21, 0x24, 0x23, 0x55, 0xa1, 0xb0, 0x34, 0x33, 0x39, 0x5b, 0x21, 0xa9, 0x28, 0xbf,
        0x3c, 0x4f, 0x21, 0x2d, 0xbf, 0x42, 0x21, 0xab, 0x34, 0xb7, 0xa0, 0x58, 0x21, 0xbf, 0x2c,
        0xb5, 0x48, 0xa1, 0x04, 0x28, 0x9d, 0x93, 0x58, 0x55, 0xa9, 0x90, 0x92, 0x9f, 0xae, 0xa7,
        0xe0, 0x31, 0xd4, 0xb4, 0x02, 0x00, 0x7e, 0x81, 0x54, 0xf9,
    ];
    const GZIP_DATA: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xf3, 0x48, 0xcd, 0xc9, 0xc9,
        0xd7, 0x51, 0x48, 0x4e, 0x4c, 0x3b, 0xbc, 0x52, 0x51, 0x21, 0x24, 0x23, 0x55, 0xa1, 0xb0,
        0x34, 0x33, 0x39, 0x5b, 0x21, 0xa9, 0x28, 0xbf, 0x3c, 0x4f, 0x21, 0x2d, 0xbf, 0x42, 0x21,
        0xab, 0x34, 0xb7, 0xa0, 0x58, 0x21, 0xbf, 0x2c, 0xb5, 0x48, 0xa1, 0x04, 0x28, 0x9d, 0x93,
        0x58, 0x55, 0xa9, 0x90, 0x92, 0x9f, 0xae, 0xa7, 0xe0, 0x31, 0xd4, 0xb4, 0x02, 0x00, 0x1d,
        0x26, 0x7f, 0x29, 0xec, 0x00, 0x00, 0x00,
    ];
    const GZIP_STORED: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x01, 0x03, 0x00, 0xfc, 0xff,
        0x41, 0x42, 0x43, 0x48, 0x03, 0x83, 0xa3, 0x03, 0x00, 0x00, 0x00,
    ];

    const DYNAMIC_DEFLATE: &[u8] = &[
        0xed, 0xcd, 0xb1, 0x15, 0xc2, 0x20, 0x18, 0x45, 0xe1, 0xde, 0x29, 0xde, 0x0a, 0xe6, 0x1c,
        0x77, 0xb0, 0xb4, 0xc8, 0x02, 0x08, 0x3f, 0x04, 0x13, 0x21, 0x06, 0x08, 0x31, 0x1b, 0x59,
        0x39, 0x44, 0x16, 0x93, 0x2d, 0x6c, 0x5e, 0x75, 0xab, 0x7b, 0xbe, 0x3c, 0x08, 0x5e, 0xc5,
        0xeb, 0x11, 0xf7, 0x25, 0xd6, 0x00, 0x1b, 0x37, 0x3c, 0xca, 0x73, 0x4e, 0x88, 0xab, 0x2c,
        0x98, 0xd4, 0xfe, 0x86, 0x89, 0x0e, 0x5a, 0xd9, 0xe3, 0x83, 0xa0, 0x8e, 0xef, 0x2a, 0xb8,
        0xf6, 0xfd, 0x0d, 0x6e, 0xf7, 0x33, 0x8c, 0xd8, 0x49, 0x65, 0x81, 0x1e, 0x4a, 0x18, 0xc5,
        0xb4, 0xaa, 0x25, 0x49, 0x46, 0xf5, 0xc1, 0xc4, 0x9a, 0x70, 0xee, 0x2e, 0x5d, 0x7b, 0x75,
        0x53, 0x24, 0x2b, 0x77, 0xca, 0xe4, 0xc8, 0x91, 0x23, 0x47, 0x8e, 0x1c, 0x39, 0x72, 0xe4,
        0xc8, 0x91, 0x23, 0xf7, 0x3f, 0xee, 0x07,
    ];

    #[test]
    fn decodes_dynamic_huffman_deflate() {
        let words = [
            "the",
            "quick",
            "brown",
            "fox",
            "jumps",
            "over",
            "lazy",
            "dog",
            "caf\u{e9}",
            "na\u{ef}ve",
            "HTTP",
            "gzip",
            "deflate",
            "chunked",
            "charset",
            "windows",
            "1252",
            "cache",
            "etag",
        ];
        let line = format!("{}\n", words.join(" "));
        let expected = line.repeat(30);
        assert_eq!(
            decode_body(Some("deflate"), DYNAMIC_DEFLATE, 8192).unwrap(),
            expected.as_bytes()
        );
    }

    #[test]
    fn decodes_raw_deflate() {
        assert_eq!(
            decode_body(Some("deflate"), RAW_DEFLATE, 4096).unwrap(),
            PLAIN
        );
    }

    #[test]
    fn decodes_zlib_wrapped_deflate() {
        assert_eq!(
            decode_body(Some("deflate"), ZLIB_DEFLATE, 4096).unwrap(),
            PLAIN
        );
    }

    #[test]
    fn decodes_gzip() {
        assert_eq!(decode_body(Some("GZip"), GZIP_DATA, 4096).unwrap(), PLAIN);
    }

    #[test]
    fn decodes_stored_gzip_block() {
        assert_eq!(
            decode_body(Some("gzip"), GZIP_STORED, 4096).unwrap(),
            b"ABC"
        );
    }

    #[test]
    fn passes_through_identity() {
        assert_eq!(decode_body(None, b"plain", 4096).unwrap(), b"plain");
        assert_eq!(
            decode_body(Some("identity"), b"plain", 4096).unwrap(),
            b"plain"
        );
    }

    #[test]
    fn rejects_brotli_and_unknown_codings() {
        assert!(matches!(
            decode_body(Some("br"), b"\x00", 4096),
            Err(HttpError::UnsupportedContentEncoding(ref name)) if name == "br"
        ));
        assert!(matches!(
            decode_body(Some("zstd"), b"\x00", 4096),
            Err(HttpError::UnsupportedContentEncoding(_))
        ));
    }

    #[test]
    fn rejects_compression_bombs() {
        assert!(matches!(
            decode_body(Some("gzip"), GZIP_DATA, 16),
            Err(HttpError::PayloadTooLarge)
        ));
    }

    #[test]
    fn rejects_corrupt_streams_with_typed_errors() {
        assert!(matches!(
            decode_body(Some("gzip"), b"not gzip", 4096),
            Err(HttpError::Decompression(_))
        ));
    }
}
