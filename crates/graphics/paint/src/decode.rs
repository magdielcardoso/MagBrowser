//! Dependency-free raster image decoding into [`ImageData`].
//!
//! The engine renders `<img>` content by decoding bytes here and handing the
//! resulting RGBA to the rasterizer. No external crates are used, so the
//! decoders are deliberately small and strict.
//!
//! # Supported formats
//!
//! * **PNG** — 8-bit, non-interlaced images of colour type 0 (grey),
//!   2 (RGB), 3 (palette), 4 (grey + alpha) and 6 (RGBA). `PLTE` and `tRNS`
//!   are honoured for palette images. The inflate implementation covers stored,
//!   fixed-Huffman and dynamic-Huffman DEFLATE blocks inside a zlib wrapper
//!   (no preset dictionaries). CRC32 is checked for every critical chunk.
//!   Interlaced (Adam7) images, bit depths other than 8 and 16-bit samples are
//!   rejected with a [`DecodeError`], never silently mis-decoded. Ancillary
//!   colour chunks (`iCCP`, `gAMA`, `cHRM`) are ignored; APNG frames are ignored
//!   and the first (default) image is returned.
//! * **BMP** — uncompressed (`BI_RGB`) 24-bit and 32-bit images, both
//!   bottom-up (positive height) and top-down (negative height). For 32-bit
//!   images the stored fourth byte is used as alpha; if every alpha byte is
//!   zero the image is treated as fully opaque, matching the common practice of
//!   BMP writers that leave the padding byte unset. Other bit depths and
//!   compressed BMP variants are reported as unsupported.
//!
//! # Unsupported formats
//!
//! GIF is detected by its header but not decoded yet (LZW decompression is not
//! implemented). JPEG and WebP are detected and reported as unsupported.
//! [`detect_format`] lets callers choose a fallback without attempting a decode.

use std::fmt;

use crate::images::ImageData;

/// Largest accepted image edge, in pixels.
///
/// Anything larger is rejected before allocating so a hostile file cannot
/// exhaust memory.
const MAX_DIMENSION: u32 = 16_384;

/// The image container formats this module can recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG (Portable Network Graphics).
    Png,
    /// GIF (Graphics Interchange Format).
    Gif,
    /// BMP (Windows bitmap).
    Bmp,
    /// JPEG (JFIF/EXIF).
    Jpeg,
    /// WebP (RIFF container).
    Webp,
}

/// An error raised while detecting or decoding image data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    /// Human-readable description of the problem.
    pub message: String,
}

impl DecodeError {
    /// Creates an error with a human-readable message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Creates an error for data that is recognised but not supported yet.
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self::new(format!("unsupported image data: {}", detail.into()))
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DecodeError {}

/// Detects the container format from magic bytes, or `None` when unknown.
pub fn detect_format(bytes: &[u8]) -> Option<ImageFormat> {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(&PNG) {
        return Some(ImageFormat::Png);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(ImageFormat::Jpeg);
    }
    if bytes.starts_with(b"BM") {
        return Some(ImageFormat::Bmp);
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(ImageFormat::Webp);
    }
    None
}

/// Decodes an image, dispatching on the magic bytes.
pub fn decode(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    match detect_format(bytes) {
        Some(ImageFormat::Png) => decode_png(bytes),
        Some(ImageFormat::Bmp) => decode_bmp(bytes),
        Some(ImageFormat::Gif) => Err(DecodeError::unsupported(
            "GIF (LZW decompression is not implemented yet)",
        )),
        Some(ImageFormat::Jpeg) => Err(DecodeError::unsupported(
            "JPEG (baseline and progressive decoding are not implemented)",
        )),
        Some(ImageFormat::Webp) => Err(DecodeError::unsupported(
            "WebP (VP8/VP8L decoding is not implemented)",
        )),
        None => Err(DecodeError::new("unrecognised image format")),
    }
}

// ---------------------------------------------------------------------------
// Checksums
// ---------------------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// DEFLATE / zlib inflate
// ---------------------------------------------------------------------------

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Reads bits from a byte slice, least-significant bit of each byte first.
struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next byte that has not been loaded into `buffer`.
    pos: usize,
    buffer: u32,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buffer: 0,
            bits: 0,
        }
    }

    /// Reads `count` bits (0..=24). At most seven bits stay buffered.
    fn read_bits(&mut self, count: u32) -> Result<u32, DecodeError> {
        while self.bits < count {
            let byte = *self
                .data
                .get(self.pos)
                .ok_or_else(|| DecodeError::new("truncated deflate stream"))?;
            self.pos += 1;
            self.buffer |= u32::from(byte) << self.bits;
            self.bits += 8;
        }
        let mask = if count == 0 { 0 } else { (1_u32 << count) - 1 };
        let value = self.buffer & mask;
        self.buffer >>= count;
        self.bits -= count;
        Ok(value)
    }

    /// Discards the partial byte so the next read is byte aligned.
    fn align_to_byte(&mut self) {
        // `read_bits` never leaves eight or more bits buffered, so this drops
        // exactly the tail of the current byte.
        self.buffer = 0;
        self.bits = 0;
    }

    /// Copies a stored block's payload, which must already be byte aligned.
    fn copy_stored(
        &mut self,
        length: usize,
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<(), DecodeError> {
        let end = self
            .pos
            .checked_add(length)
            .ok_or_else(|| DecodeError::new("truncated deflate stream"))?;
        if end > self.data.len() {
            return Err(DecodeError::new("truncated deflate stream"));
        }
        if out.len().saturating_add(length) > limit {
            return Err(DecodeError::new("decompressed image data is too large"));
        }
        out.extend_from_slice(&self.data[self.pos..end]);
        self.pos = end;
        Ok(())
    }
}

/// Canonical Huffman decoding table (RFC 1951, section 3.2.2).
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> Result<Self, DecodeError> {
        let mut counts = [0_u16; 16];
        for &length in lengths {
            if length > 15 {
                return Err(DecodeError::new("invalid Huffman code length"));
            }
            if length > 0 {
                counts[usize::from(length)] += 1;
            }
        }
        // Reject over-subscribed code sets; incomplete sets are legal.
        let mut left: i32 = 1;
        for &count in counts.iter().skip(1) {
            left = left * 2 - i32::from(count);
            if left < 0 {
                return Err(DecodeError::new("over-subscribed Huffman code"));
            }
        }
        let total: usize = lengths.iter().filter(|length| **length > 0).count();
        let mut offsets = [0_u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0_u16; total];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length > 0 {
                let slot = usize::from(offsets[usize::from(length)]);
                if slot >= symbols.len() {
                    return Err(DecodeError::new("over-subscribed Huffman code"));
                }
                symbols[slot] = u16::try_from(symbol)
                    .map_err(|_| DecodeError::new("invalid Huffman symbol"))?;
                offsets[usize::from(length)] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, DecodeError> {
        let mut code: u32 = 0;
        let mut first: u32 = 0;
        let mut index: usize = 0;
        for length in 1..=15 {
            code = (code << 1) | reader.read_bits(1)?;
            let count = u32::from(self.counts[length]);
            if code >= first && code - first < count {
                let slot = index + (code - first) as usize;
                return self
                    .symbols
                    .get(slot)
                    .copied()
                    .ok_or_else(|| DecodeError::new("invalid Huffman code"));
            }
            index += count as usize;
            first = (first + count) << 1;
        }
        Err(DecodeError::new("invalid Huffman code"))
    }
}

fn fixed_tables() -> Result<(Huffman, Huffman), DecodeError> {
    let mut literal_lengths = [0_u8; 288];
    for (symbol, length) in literal_lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let distance_lengths = [5_u8; 32];
    Ok((
        Huffman::from_lengths(&literal_lengths)?,
        Huffman::from_lengths(&distance_lengths)?,
    ))
}

fn dynamic_tables(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman), DecodeError> {
    let literal_count = reader.read_bits(5)? as usize + 257;
    let distance_count = reader.read_bits(5)? as usize + 1;
    let code_length_count = reader.read_bits(4)? as usize + 4;
    if literal_count > 286 {
        return Err(DecodeError::new("too many literal/length codes"));
    }
    if distance_count > 30 {
        return Err(DecodeError::new("too many distance codes"));
    }
    let mut code_lengths = [0_u8; 19];
    for &slot in CODE_LENGTH_ORDER.iter().take(code_length_count) {
        code_lengths[slot] = reader.read_bits(3)? as u8;
    }
    if code_lengths[16..=18].iter().any(|&length| length > 7) {
        return Err(DecodeError::new("invalid code length code length"));
    }
    let code_length_table = Huffman::from_lengths(&code_lengths)?;

    let mut lengths = vec![0_u8; literal_count + distance_count];
    let mut index = 0;
    while index < lengths.len() {
        let symbol = code_length_table.decode(reader)?;
        match symbol {
            0..=15 => {
                lengths[index] = symbol as u8;
                index += 1;
            }
            16..=18 => {
                let (repeat_base, extra_bits) = match symbol {
                    16 => (3_usize, 2_u32),
                    17 => (3, 3),
                    _ => (11, 7),
                };
                let previous = if symbol == 16 {
                    if index == 0 {
                        return Err(DecodeError::new("repeat code without a previous length"));
                    }
                    lengths[index - 1]
                } else {
                    0
                };
                let repeat = repeat_base + reader.read_bits(extra_bits)? as usize;
                let end = index
                    .checked_add(repeat)
                    .ok_or_else(|| DecodeError::new("invalid code length repeat"))?;
                if end > lengths.len() {
                    return Err(DecodeError::new("code length repeat runs past the table"));
                }
                for slot in &mut lengths[index..end] {
                    *slot = previous;
                }
                index = end;
            }
            _ => return Err(DecodeError::new("invalid code length symbol")),
        }
    }
    if lengths[256] == 0 {
        return Err(DecodeError::new("missing end-of-block code"));
    }
    let literal_table = Huffman::from_lengths(&lengths[..literal_count])?;
    let distance_table = Huffman::from_lengths(&lengths[literal_count..])?;
    Ok((literal_table, distance_table))
}

fn inflate_block(
    reader: &mut BitReader<'_>,
    literals: &Huffman,
    distances: &Huffman,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<(), DecodeError> {
    loop {
        let symbol = literals.decode(reader)?;
        match symbol {
            0..=255 => {
                if out.len() >= limit {
                    return Err(DecodeError::new("decompressed image data is too large"));
                }
                out.push(symbol as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let slot = usize::from(symbol - 257);
                let extra = u32::from(LENGTH_EXTRA[slot]);
                let length = usize::from(LENGTH_BASE[slot]) + reader.read_bits(extra)? as usize;
                let distance_symbol = distances.decode(reader)? as usize;
                if distance_symbol >= DISTANCE_BASE.len() {
                    return Err(DecodeError::new("invalid distance code"));
                }
                let extra = u32::from(DISTANCE_EXTRA[distance_symbol]);
                let distance =
                    usize::from(DISTANCE_BASE[distance_symbol]) + reader.read_bits(extra)? as usize;
                if distance > out.len() {
                    return Err(DecodeError::new("back-reference before start of output"));
                }
                if out.len().saturating_add(length) > limit {
                    return Err(DecodeError::new("decompressed image data is too large"));
                }
                let start = out.len() - distance;
                for offset in 0..length {
                    let byte = out[start + offset];
                    out.push(byte);
                }
            }
            _ => return Err(DecodeError::new("invalid literal/length symbol")),
        }
    }
}

/// Inflates a raw DEFLATE stream, returning the output and bytes consumed.
fn inflate(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), DecodeError> {
    let mut reader = BitReader::new(data);
    let mut out = Vec::new();
    loop {
        let final_block = reader.read_bits(1)? == 1;
        match reader.read_bits(2)? {
            0 => {
                reader.align_to_byte();
                let length = reader.read_bits(16)? as usize;
                let complement = reader.read_bits(16)? as usize;
                if length != (!complement) & 0xffff {
                    return Err(DecodeError::new("stored block length mismatch"));
                }
                reader.copy_stored(length, &mut out, limit)?;
            }
            1 => {
                let (literals, distances) = fixed_tables()?;
                inflate_block(&mut reader, &literals, &distances, &mut out, limit)?;
            }
            2 => {
                let (literals, distances) = dynamic_tables(&mut reader)?;
                inflate_block(&mut reader, &literals, &distances, &mut out, limit)?;
            }
            _ => return Err(DecodeError::new("invalid deflate block type")),
        }
        if final_block {
            break;
        }
    }
    Ok((out, reader.pos))
}

/// Inflates a zlib stream (RFC 1950) and verifies its Adler-32 checksum.
fn inflate_zlib(data: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    if data.len() < 2 {
        return Err(DecodeError::new("truncated zlib header"));
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0f != 8 {
        return Err(DecodeError::new("unsupported zlib compression method"));
    }
    if cmf >> 4 > 7 {
        return Err(DecodeError::new("invalid zlib window size"));
    }
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err(DecodeError::new("invalid zlib header checksum"));
    }
    if flg & 0x20 != 0 {
        return Err(DecodeError::unsupported(
            "zlib streams with a preset dictionary",
        ));
    }
    let (out, consumed) = inflate(&data[2..], limit)?;
    let checksum_start = 2 + consumed;
    let checksum_end = checksum_start + 4;
    if checksum_end > data.len() {
        return Err(DecodeError::new("truncated zlib checksum"));
    }
    let expected = u32::from_be_bytes([
        data[checksum_start],
        data[checksum_start + 1],
        data[checksum_start + 2],
        data[checksum_start + 3],
    ]);
    if adler32(&out) != expected {
        return Err(DecodeError::new("zlib checksum mismatch"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

struct PngHeader {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
}

fn parse_png_header(data: &[u8]) -> Result<PngHeader, DecodeError> {
    if data.len() != 13 {
        return Err(DecodeError::new("invalid IHDR chunk length"));
    }
    let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if width == 0 || height == 0 {
        return Err(DecodeError::new("PNG has a zero dimension"));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(DecodeError::new(format!(
            "PNG dimensions {width}x{height} exceed the {MAX_DIMENSION} pixel limit"
        )));
    }
    let bit_depth = data[8];
    let color_type = data[9];
    if data[10] != 0 {
        return Err(DecodeError::new("unsupported PNG compression method"));
    }
    if data[11] != 0 {
        return Err(DecodeError::new("unsupported PNG filter method"));
    }
    if data[12] > 1 {
        return Err(DecodeError::new("unsupported PNG interlace method"));
    }
    Ok(PngHeader {
        width,
        height,
        bit_depth,
        color_type,
        interlace: data[12],
    })
}

/// Decodes a PNG image into RGBA8.
///
/// Interlaced (Adam7) images are rejected with a [`DecodeError`], as are bit
/// depths other than 8 and colour types not listed in the module docs.
pub fn decode_png(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    if !bytes.starts_with(&PNG_SIGNATURE) {
        return Err(DecodeError::new("not a PNG file"));
    }
    let mut pos = PNG_SIGNATURE.len();
    let mut header: Option<PngHeader> = None;
    let mut palette: Option<Vec<[u8; 3]>> = None;
    let mut transparency: Vec<u8> = Vec::new();
    let mut compressed: Vec<u8> = Vec::new();
    let mut seen_end = false;

    while pos < bytes.len() {
        if bytes.len() - pos < 8 {
            return Err(DecodeError::new("truncated PNG chunk header"));
        }
        let length_bytes = &bytes[pos..pos + 4];
        let length = u32::from_be_bytes([
            length_bytes[0],
            length_bytes[1],
            length_bytes[2],
            length_bytes[3],
        ]) as usize;
        let kind: [u8; 4] = bytes[pos + 4..pos + 8]
            .try_into()
            .map_err(|_| DecodeError::new("truncated PNG chunk type"))?;
        let data_start = pos
            .checked_add(8)
            .ok_or_else(|| DecodeError::new("PNG chunk length overflow"))?;
        let data_end = data_start
            .checked_add(length)
            .ok_or_else(|| DecodeError::new("PNG chunk length overflow"))?;
        let crc_end = data_end
            .checked_add(4)
            .ok_or_else(|| DecodeError::new("PNG chunk length overflow"))?;
        if crc_end > bytes.len() {
            return Err(DecodeError::new("truncated PNG chunk data"));
        }
        let data = &bytes[data_start..data_end];
        let stored_crc = u32::from_be_bytes([
            bytes[data_end],
            bytes[data_end + 1],
            bytes[data_end + 2],
            bytes[data_end + 3],
        ]);
        let critical = kind[0].is_ascii_uppercase();
        if critical {
            let mut crc_input = Vec::with_capacity(4 + data.len());
            crc_input.extend_from_slice(&kind);
            crc_input.extend_from_slice(data);
            if crc32(&crc_input) != stored_crc {
                return Err(DecodeError::new(format!(
                    "CRC mismatch in PNG {} chunk",
                    String::from_utf8_lossy(&kind)
                )));
            }
        }
        pos = crc_end;

        match &kind {
            b"IHDR" => {
                if header.is_some() {
                    return Err(DecodeError::new("duplicate IHDR chunk"));
                }
                header = Some(parse_png_header(data)?);
            }
            b"PLTE" => {
                if !data.len().is_multiple_of(3) || data.is_empty() || data.len() > 3 * 256 {
                    return Err(DecodeError::new("invalid PLTE chunk length"));
                }
                let entries = data
                    .chunks(3)
                    .map(|chunk| [chunk[0], chunk[1], chunk[2]])
                    .collect();
                palette = Some(entries);
            }
            b"tRNS" => transparency = data.to_vec(),
            b"IDAT" => compressed.extend_from_slice(data),
            b"IEND" => {
                seen_end = true;
                break;
            }
            _ => {
                if critical {
                    return Err(DecodeError::unsupported(format!(
                        "critical PNG chunk {}",
                        String::from_utf8_lossy(&kind)
                    )));
                }
            }
        }
    }

    let header = header.ok_or_else(|| DecodeError::new("missing IHDR chunk"))?;
    if !seen_end {
        return Err(DecodeError::new("missing IEND chunk (truncated PNG)"));
    }
    if compressed.is_empty() {
        return Err(DecodeError::new("missing IDAT chunk"));
    }
    if header.interlace != 0 {
        return Err(DecodeError::unsupported("interlaced (Adam7) PNG images"));
    }
    if header.bit_depth != 8 {
        return Err(DecodeError::unsupported(format!(
            "PNG bit depth {} (only 8-bit samples are supported)",
            header.bit_depth
        )));
    }
    let channels = match header.color_type {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        other => {
            return Err(DecodeError::unsupported(format!("PNG colour type {other}")));
        }
    };
    if header.color_type == 3 && palette.is_none() {
        return Err(DecodeError::new("palette PNG is missing its PLTE chunk"));
    }

    let width = header.width as usize;
    let height = header.height as usize;
    let stride = width
        .checked_mul(channels)
        .ok_or_else(|| DecodeError::new("PNG row size overflow"))?;
    let raw_length = stride
        .checked_add(1)
        .and_then(|row| row.checked_mul(height))
        .ok_or_else(|| DecodeError::new("PNG image data size overflow"))?;
    let raw = inflate_zlib(&compressed, raw_length)?;
    if raw.len() != raw_length {
        return Err(DecodeError::new(
            "PNG image data does not match its declared size",
        ));
    }

    let mut pixels = Vec::with_capacity(stride * height);
    let mut previous = vec![0_u8; stride];
    let mut current = vec![0_u8; stride];
    for row in 0..height {
        let start = row * (stride + 1);
        let filter = raw[start];
        current.copy_from_slice(&raw[start + 1..start + 1 + stride]);
        unfilter(filter, &mut current, &previous, channels)?;

        match header.color_type {
            0 => {
                for &value in &current {
                    pixels.extend_from_slice(&[value, value, value, 255]);
                }
            }
            2 => {
                for pixel in current.chunks(3) {
                    pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
            }
            3 => {
                let table = palette
                    .as_ref()
                    .ok_or_else(|| DecodeError::new("palette PNG is missing its PLTE chunk"))?;
                for &index in &current {
                    let entry = table.get(usize::from(index)).ok_or_else(|| {
                        DecodeError::new("palette index is outside the PLTE chunk")
                    })?;
                    let alpha = transparency.get(usize::from(index)).copied().unwrap_or(255);
                    pixels.extend_from_slice(&[entry[0], entry[1], entry[2], alpha]);
                }
            }
            4 => {
                for pixel in current.chunks(2) {
                    pixels.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
                }
            }
            _ => {
                // Colour type 6: already RGBA.
                pixels.extend_from_slice(&current);
            }
        }

        std::mem::swap(&mut previous, &mut current);
    }

    ImageData::from_rgba(header.width, header.height, pixels)
}

fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[allow(clippy::needless_range_loop)]
fn unfilter(
    filter: u8,
    row: &mut [u8],
    previous: &[u8],
    bytes_per_pixel: usize,
) -> Result<(), DecodeError> {
    match filter {
        0 => {}
        1 => {
            for index in bytes_per_pixel..row.len() {
                row[index] = row[index].wrapping_add(row[index - bytes_per_pixel]);
            }
        }
        2 => {
            for index in 0..row.len() {
                row[index] = row[index].wrapping_add(previous[index]);
            }
        }
        3 => {
            for index in 0..row.len() {
                let left = if index >= bytes_per_pixel {
                    u16::from(row[index - bytes_per_pixel])
                } else {
                    0
                };
                let up = u16::from(previous[index]);
                row[index] = row[index].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for index in 0..row.len() {
                let left = if index >= bytes_per_pixel {
                    row[index - bytes_per_pixel]
                } else {
                    0
                };
                let up = previous[index];
                let up_left = if index >= bytes_per_pixel {
                    previous[index - bytes_per_pixel]
                } else {
                    0
                };
                row[index] = row[index].wrapping_add(paeth_predictor(left, up, up_left));
            }
        }
        other => {
            return Err(DecodeError::new(format!("invalid PNG filter type {other}")));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// BMP
// ---------------------------------------------------------------------------

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, DecodeError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| DecodeError::new("truncated BMP header"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, DecodeError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| DecodeError::new("truncated BMP header"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_i32_le(bytes: &[u8], offset: usize) -> Result<i32, DecodeError> {
    Ok(read_u32_le(bytes, offset)? as i32)
}

/// Decodes an uncompressed (BI_RGB) 24-bit or 32-bit BMP into RGBA8.
fn decode_bmp(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    if !bytes.starts_with(b"BM") {
        return Err(DecodeError::new("not a BMP file"));
    }
    if bytes.len() < 14 + 40 {
        return Err(DecodeError::new("truncated BMP header"));
    }
    let pixel_offset = read_u32_le(bytes, 10)? as usize;
    let header_size = read_u32_le(bytes, 14)?;
    if header_size < 40 {
        return Err(DecodeError::unsupported(
            "BMP headers smaller than BITMAPINFOHEADER",
        ));
    }
    let width = read_i32_le(bytes, 18)?;
    let height = read_i32_le(bytes, 22)?;
    let planes = read_u16_le(bytes, 26)?;
    let bits_per_pixel = read_u16_le(bytes, 28)?;
    let compression = read_u32_le(bytes, 30)?;
    if planes != 1 {
        return Err(DecodeError::new("invalid BMP plane count"));
    }
    if compression != 0 {
        return Err(DecodeError::unsupported(
            "compressed BMP images (only BI_RGB is supported)",
        ));
    }
    if width <= 0 || height == 0 {
        return Err(DecodeError::new("invalid BMP dimensions"));
    }
    let width = width as u32;
    let top_down = height < 0;
    let height = height.unsigned_abs();
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(DecodeError::new(format!(
            "BMP dimensions {width}x{height} exceed the {MAX_DIMENSION} pixel limit"
        )));
    }
    if bits_per_pixel != 24 && bits_per_pixel != 32 {
        return Err(DecodeError::unsupported(format!(
            "{bits_per_pixel}-bit BMP images (only 24-bit and 32-bit are supported)"
        )));
    }

    let bytes_per_pixel = usize::from(bits_per_pixel / 8);
    let width_usize = width as usize;
    let height_usize = height as usize;
    let row_bytes = width_usize
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| DecodeError::new("BMP row size overflow"))?;
    // Rows are padded to a four-byte boundary.
    let row_stride = row_bytes
        .checked_add(3)
        .map(|value| value & !3)
        .ok_or_else(|| DecodeError::new("BMP row size overflow"))?;
    let data_end = row_stride
        .checked_mul(height_usize)
        .and_then(|size| pixel_offset.checked_add(size))
        .ok_or_else(|| DecodeError::new("BMP image data size overflow"))?;
    if data_end > bytes.len() {
        return Err(DecodeError::new("truncated BMP pixel data"));
    }

    let mut pixels = vec![0_u8; width_usize * height_usize * 4];
    let mut any_alpha = false;
    for y in 0..height_usize {
        let source_row = if top_down { y } else { height_usize - 1 - y };
        let start = pixel_offset + source_row * row_stride;
        let row = &bytes[start..start + row_bytes];
        let destination = &mut pixels[y * width_usize * 4..(y + 1) * width_usize * 4];
        for x in 0..width_usize {
            let source = &row[x * bytes_per_pixel..(x + 1) * bytes_per_pixel];
            let alpha = if bytes_per_pixel == 4 { source[3] } else { 255 };
            any_alpha |= alpha != 0;
            let target = &mut destination[x * 4..(x + 1) * 4];
            target[0] = source[2];
            target[1] = source[1];
            target[2] = source[0];
            target[3] = alpha;
        }
    }
    if bytes_per_pixel == 4 && !any_alpha {
        // Many writers leave the fourth byte unset; treat such images as opaque.
        for pixel in pixels.chunks_mut(4) {
            pixel[3] = 255;
        }
    }
    ImageData::from_rgba(width, height, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::png;

    fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }

    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        if data.is_empty() {
            out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
        }
        let mut chunks = data.chunks(65535).peekable();
        while let Some(chunk) = chunks.next() {
            out.push(u8::from(chunks.peek().is_none()));
            let length = chunk.len() as u16;
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&(!length).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    fn assemble_png(
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
        extra: &[(&[u8; 4], &[u8])],
        idat: &[u8],
    ) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
        write_chunk(&mut out, b"IHDR", &header);
        for (kind, data) in extra {
            write_chunk(&mut out, kind, data);
        }
        write_chunk(&mut out, b"IDAT", idat);
        write_chunk(&mut out, b"IEND", &[]);
        out
    }

    fn png_with_raw(
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
        extra: &[(&[u8; 4], &[u8])],
        raw: &[u8],
    ) -> Vec<u8> {
        assemble_png(
            width,
            height,
            bit_depth,
            color_type,
            extra,
            &zlib_stored(raw),
        )
    }

    #[test]
    fn roundtrips_encoder_output() {
        for (width, height) in [(1_u32, 1_u32), (3, 5), (50, 40)] {
            let mut rgba = Vec::with_capacity((width * height * 4) as usize);
            for y in 0..height {
                for x in 0..width {
                    let r = (x * 5) as u8;
                    let g = (y * 6) as u8;
                    let b = (x + y) as u8;
                    let a = if (x + y) % 3 == 0 { 128 } else { 255 };
                    rgba.extend_from_slice(&[r, g, b, a]);
                }
            }
            let encoded = png::encode(width, height, &rgba);
            let image = decode_png(&encoded).expect("encoded PNG should decode");
            assert_eq!(image.width, width);
            assert_eq!(image.height, height);
            assert_eq!(image.pixels, rgba, "pixels differ for {width}x{height}");
            assert_eq!(
                ImageData::decode(&encoded).expect("convenience decode"),
                image
            );
        }
    }

    #[test]
    fn decodes_palette_with_transparency() {
        let palette: [[u8; 3]; 3] = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
        let mut plte = Vec::new();
        for entry in palette {
            plte.extend_from_slice(&entry);
        }
        let raw = [0_u8, 0, 1, 2]; // filter byte followed by three indices
        let file = png_with_raw(
            3,
            1,
            8,
            3,
            &[(b"PLTE", &plte), (b"tRNS", &[0, 128, 255])],
            &raw,
        );
        let image = decode_png(&file).expect("palette PNG should decode");
        assert_eq!(image.get(0, 0), [255, 0, 0, 0]);
        assert_eq!(image.get(1, 0), [0, 255, 0, 128]);
        assert_eq!(image.get(2, 0), [0, 0, 255, 255]);
    }

    #[test]
    fn decodes_grey_and_grey_alpha() {
        let raw = [0_u8, 10, 20, 0, 30, 40];
        let file = png_with_raw(2, 2, 8, 0, &[], &raw);
        let image = decode_png(&file).expect("grey PNG should decode");
        assert_eq!(image.get(0, 0), [10, 10, 10, 255]);
        assert_eq!(image.get(1, 0), [20, 20, 20, 255]);
        assert_eq!(image.get(0, 1), [30, 30, 30, 255]);
        assert_eq!(image.get(1, 1), [40, 40, 40, 255]);

        let raw = [0_u8, 1, 2, 3, 4];
        let file = png_with_raw(2, 1, 8, 4, &[], &raw);
        let image = decode_png(&file).expect("grey+alpha PNG should decode");
        assert_eq!(image.get(0, 0), [1, 1, 1, 2]);
        assert_eq!(image.get(1, 0), [3, 3, 3, 4]);
    }

    #[test]
    fn decodes_rgb() {
        let raw = [0_u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let file = png_with_raw(3, 1, 8, 2, &[], &raw);
        let image = decode_png(&file).expect("RGB PNG should decode");
        assert_eq!(image.get(0, 0), [1, 2, 3, 255]);
        assert_eq!(image.get(1, 0), [4, 5, 6, 255]);
        assert_eq!(image.get(2, 0), [7, 8, 9, 255]);
    }

    fn filter_row(filter: u8, row: &[u8], previous: &[u8], bpp: usize) -> Vec<u8> {
        let mut out = vec![filter];
        for index in 0..row.len() {
            let left = if index >= bpp { row[index - bpp] } else { 0 };
            let up = previous[index];
            let up_left = if index >= bpp {
                previous[index - bpp]
            } else {
                0
            };
            let predictor = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth_predictor(left, up, up_left),
                _ => unreachable!(),
            };
            out.push(row[index].wrapping_sub(predictor));
        }
        out
    }

    #[test]
    fn decodes_each_png_filter_type() {
        let width = 4_usize;
        let height = 5_usize;
        let bpp = 4_usize;
        let rows: Vec<Vec<u8>> = (0..height)
            .map(|row| {
                (0..width * bpp)
                    .map(|index| (row * 31 + index * 7) as u8)
                    .collect()
            })
            .collect();
        let mut raw = Vec::new();
        let mut previous = vec![0_u8; width * bpp];
        for (row_index, row) in rows.iter().enumerate() {
            let filter = row_index as u8; // one row per filter type
            raw.extend_from_slice(&filter_row(filter, row, &previous, bpp));
            previous = row.clone();
        }
        let file = png_with_raw(width as u32, height as u32, 8, 6, &[], &raw);
        let image = decode_png(&file).expect("filtered PNG should decode");
        for (y, row) in rows.iter().enumerate() {
            for x in 0..width {
                let offset = x * bpp;
                let expected = [
                    row[offset],
                    row[offset + 1],
                    row[offset + 2],
                    row[offset + 3],
                ];
                assert_eq!(image.get(x as u32, y as u32), expected, "at {x},{y}");
            }
        }
    }

    struct BitWriter {
        out: Vec<u8>,
        bit: u32,
        count: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                bit: 0,
                count: 0,
            }
        }

        fn push_bit(&mut self, value: u8) {
            self.bit |= u32::from(value) << self.count;
            self.count += 1;
            if self.count == 8 {
                self.out.push(self.bit as u8);
                self.bit = 0;
                self.count = 0;
            }
        }

        /// Writes a value least-significant bit first (DEFLATE integers).
        fn write(&mut self, value: u32, bits: u32) {
            for index in 0..bits {
                self.push_bit(((value >> index) & 1) as u8);
            }
        }

        /// Writes a Huffman code most-significant bit first.
        fn write_code(&mut self, code: u32, bits: u32) {
            for index in (0..bits).rev() {
                self.push_bit(((code >> index) & 1) as u8);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                self.out.push(self.bit as u8);
            }
            self.out
        }
    }

    fn deflate_fixed_literals(data: &[u8]) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.write(1, 1); // BFINAL
        writer.write(1, 2); // BTYPE = fixed Huffman
        for &byte in data {
            if byte < 144 {
                writer.write_code(0x30 + u32::from(byte), 8);
            } else {
                writer.write_code(0x190 + u32::from(byte) - 144, 9);
            }
        }
        writer.write_code(0, 7); // end of block
        writer.finish()
    }

    fn deflate_dynamic_literals(data: &[u8]) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.write(1, 1); // BFINAL
        writer.write(2, 2); // BTYPE = dynamic Huffman
        writer.write(0, 5); // HLIT: 257 literal/length codes
        writer.write(0, 5); // HDIST: one distance code
        writer.write(15, 4); // HCLEN: all 19 code-length codes present
        // Code-length alphabet: symbol 8 gets length 1, symbols 1 and 9 length 2.
        let mut code_lengths = [0_u8; 19];
        code_lengths[4] = 1; // order slot of symbol 8
        code_lengths[6] = 2; // order slot of symbol 9
        code_lengths[17] = 2; // order slot of symbol 1
        for value in code_lengths {
            writer.write(u32::from(value), 3);
        }
        // Literal lengths 0..=254 are 8 bits, 255 and 256 are 9 bits, and the
        // single distance code is 1 bit.
        for _ in 0..255 {
            writer.write_code(0, 1); // code length 8
        }
        writer.write_code(0b11, 2); // code length 9 (symbol 255)
        writer.write_code(0b11, 2); // code length 9 (symbol 256)
        writer.write_code(0b10, 2); // distance code length 1
        for &byte in data {
            if byte < 255 {
                writer.write_code(u32::from(byte), 8);
            } else {
                writer.write_code(510, 9);
            }
        }
        writer.write_code(511, 9); // end of block for symbol 256
        writer.finish()
    }

    fn zlib_wrap(deflate: &[u8], raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        out.extend_from_slice(deflate);
        out.extend_from_slice(&adler32(raw).to_be_bytes());
        out
    }

    #[test]
    fn inflates_fixed_and_dynamic_huffman_blocks() {
        // Valid PNG scanlines: each row is one filter byte (None) plus 120 bytes.
        let mut raw = Vec::new();
        for row in 0..20 {
            raw.push(0_u8);
            for column in 0..120 {
                raw.push(((row * 17 + column * 13) % 256) as u8);
            }
        }
        for deflate in [deflate_fixed_literals(&raw), deflate_dynamic_literals(&raw)] {
            let zlib = zlib_wrap(&deflate, &raw);
            let out = inflate_zlib(&zlib, raw.len()).expect("hand-built stream should inflate");
            assert_eq!(out, raw);
            let file = assemble_png(30, 20, 8, 6, &[], &zlib);
            let image = decode_png(&file).expect("PNG with Huffman-coded IDAT");
            let expected: Vec<u8> = raw.chunks(121).flat_map(|row| row[1..].to_vec()).collect();
            assert_eq!(image.pixels, expected);
        }
    }

    #[test]
    fn inflates_stored_blocks_across_chunk_boundaries() {
        let raw = vec![9_u8; 200_000];
        let zlib = zlib_stored(&raw);
        let out = inflate_zlib(&zlib, raw.len()).expect("stored blocks should inflate");
        assert_eq!(out, raw);
    }

    #[test]
    fn rejects_truncated_and_corrupt_streams() {
        let width = 8_u32;
        let height = 8_u32;
        let rgba = vec![7_u8; (width * height * 4) as usize];
        let encoded = png::encode(width, height, &rgba);

        let header_len = 8 + 25;
        let truncated = &encoded[..header_len + 10];
        assert!(decode_png(truncated).is_err());

        let empty: &[u8] = &[];
        assert!(decode(empty).is_err());
        assert!(decode_png(empty).is_err());

        let zlib = zlib_stored(&[0_u8; 16]);
        let mut corrupt_header = zlib.clone();
        corrupt_header[0] = 0x79;
        assert!(inflate_zlib(&corrupt_header, 16).is_err());

        let mut bad_checksum = zlib.clone();
        *bad_checksum.last_mut().expect("checksum bytes") ^= 0xff;
        assert!(inflate_zlib(&bad_checksum, 16).is_err());

        let mut too_large = zlib.clone();
        assert!(inflate_zlib(&too_large, 4).is_err());
        too_large.truncate(6);
        assert!(inflate_zlib(&too_large, 16).is_err());
    }

    #[test]
    fn rejects_bad_png_crc() {
        let mut file = png_with_raw(2, 1, 8, 2, &[], &[0, 1, 2, 3, 4, 5, 6]);
        let idat = file
            .windows(4)
            .position(|window| window == b"IDAT")
            .expect("IDAT present");
        // Flip a payload byte; the chunk CRC must no longer match.
        file[idat + 4] ^= 0x40;
        let error = decode_png(&file).expect_err("bad CRC must be rejected");
        assert!(error.message.contains("CRC"));
    }

    #[test]
    fn rejects_bogus_dimensions() {
        let file = png_with_raw(50_000, 1, 8, 2, &[], &[0, 1, 2, 3]);
        assert!(decode_png(&file).is_err());

        let zero = png_with_raw(0, 1, 8, 2, &[], &[]);
        assert!(decode_png(&zero).is_err());

        let wrong_size = png_with_raw(4, 4, 8, 6, &[], &[0, 1, 2, 3]);
        assert!(decode_png(&wrong_size).is_err());
    }

    #[test]
    fn rejects_unsupported_png_variants() {
        // Interlaced.
        let mut interlaced = png_with_raw(1, 1, 8, 6, &[], &[0, 1, 2, 3, 4]);
        let ihdr_data = 8 + 8;
        interlaced[ihdr_data + 12] = 1;
        // Repair the IHDR CRC after editing the interlace byte.
        let crc_start = 8 + 8 + 13;
        let mut crc_input = Vec::new();
        crc_input.extend_from_slice(b"IHDR");
        crc_input.extend_from_slice(&interlaced[ihdr_data..ihdr_data + 13]);
        let crc = crc32(&crc_input).to_be_bytes();
        interlaced[crc_start..crc_start + 4].copy_from_slice(&crc);
        assert!(decode_png(&interlaced).is_err());

        // 16-bit samples.
        let deep = png_with_raw(1, 1, 16, 0, &[], &[0, 0, 0]);
        assert!(decode_png(&deep).is_err());

        // Palette image without PLTE.
        let no_palette = png_with_raw(1, 1, 8, 3, &[], &[0, 0]);
        assert!(decode_png(&no_palette).is_err());

        // Palette index past the table.
        let palette = [1_u8, 2, 3];
        let out_of_range = png_with_raw(1, 1, 8, 3, &[(b"PLTE", &palette)], &[0, 7]);
        assert!(decode_png(&out_of_range).is_err());
    }

    fn bmp24(width: u32, rows: &[[[u8; 3]; 3]], top_down: bool) -> Vec<u8> {
        let height = rows.len() as u32;
        let row_bytes = width as usize * 3;
        let stride = (row_bytes + 3) & !3;
        let pixel_offset = 14 + 40;
        let mut out = Vec::new();
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
        out.extend_from_slice(&40_u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        let signed_height = if top_down {
            -(height as i32)
        } else {
            height as i32
        };
        out.extend_from_slice(&signed_height.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&24_u16.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&(stride * height as usize).to_le_bytes()[..4]);
        out.extend_from_slice(&0_i32.to_le_bytes());
        out.extend_from_slice(&0_i32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        let ordered: Vec<&[[u8; 3]; 3]> = if top_down {
            rows.iter().collect()
        } else {
            rows.iter().rev().collect()
        };
        for row in ordered {
            let mut written = 0;
            for pixel in row.iter().take(width as usize) {
                out.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                written += 3;
            }
            while written < stride {
                out.push(0);
                written += 1;
            }
        }
        out
    }

    #[test]
    fn decodes_24_bit_bmp_both_orientations() {
        let red = [255_u8, 0, 0];
        let green = [0_u8, 255, 0];
        let blue = [0_u8, 0, 255];
        let rows: [[[u8; 3]; 3]; 2] = [[red, green, blue], [blue, red, green]];
        for top_down in [false, true] {
            let file = bmp24(3, &rows, top_down);
            let image = decode(&file).expect("BMP should decode");
            assert_eq!(image.width, 3);
            assert_eq!(image.height, 2);
            assert_eq!(image.get(0, 0), [255, 0, 0, 255]);
            assert_eq!(image.get(1, 0), [0, 255, 0, 255]);
            assert_eq!(image.get(2, 0), [0, 0, 255, 255]);
            assert_eq!(image.get(0, 1), [0, 0, 255, 255]);
            assert_eq!(image.get(1, 1), [255, 0, 0, 255]);
            assert_eq!(image.get(2, 1), [0, 255, 0, 255]);
        }
    }

    fn bmp32(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let pixel_offset = 14 + 40;
        let row_bytes = width as usize * 4;
        let mut out = Vec::new();
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
        out.extend_from_slice(&40_u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        out.extend_from_slice(&(-(height as i32)).to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&32_u16.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&(row_bytes * height as usize).to_le_bytes()[..4]);
        out.extend_from_slice(&0_i32.to_le_bytes());
        out.extend_from_slice(&0_i32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        for pixel in pixels {
            out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        out
    }

    #[test]
    fn decodes_32_bit_bmp_alpha() {
        let pixels = [[1_u8, 2, 3, 200], [4, 5, 6, 100]];
        let file = bmp32(2, 1, &pixels);
        let image = decode(&file).expect("32-bit BMP should decode");
        assert_eq!(image.get(0, 0), [1, 2, 3, 200]);
        assert_eq!(image.get(1, 0), [4, 5, 6, 100]);

        // An all-zero alpha channel is treated as opaque.
        let opaque_pixels = [[1_u8, 2, 3, 0], [4, 5, 6, 0]];
        let file = bmp32(2, 1, &opaque_pixels);
        let image = decode(&file).expect("32-bit BMP should decode");
        assert_eq!(image.get(0, 0), [1, 2, 3, 255]);
        assert_eq!(image.get(1, 0), [4, 5, 6, 255]);
    }

    #[test]
    fn rejects_unsupported_bmp_variants() {
        let mut file = bmp24(1, &[[[1, 2, 3], [1, 2, 3], [1, 2, 3]]], false);
        // 8-bit BMP.
        file[28] = 8;
        assert!(decode(&file).is_err());

        // Truncated pixel data.
        let mut file = bmp24(1, &[[[1, 2, 3], [1, 2, 3], [1, 2, 3]]], false);
        file.truncate(file.len() - 1);
        assert!(decode(&file).is_err());

        // Compressed BMP.
        let mut file = bmp24(1, &[[[1, 2, 3], [1, 2, 3], [1, 2, 3]]], false);
        file[30] = 1;
        assert!(decode(&file).is_err());
    }

    #[test]
    fn detects_magic_bytes() {
        let png = png::encode(1, 1, &[1, 2, 3, 4]);
        assert_eq!(detect_format(&png), Some(ImageFormat::Png));
        assert_eq!(detect_format(b"GIF89a\x01\x00"), Some(ImageFormat::Gif));
        assert_eq!(detect_format(b"GIF87a"), Some(ImageFormat::Gif));
        assert_eq!(
            detect_format(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(detect_format(b"BM\x00\x00"), Some(ImageFormat::Bmp));
        assert_eq!(
            detect_format(b"RIFF\x04\x00\x00\x00WEBPVP8 "),
            Some(ImageFormat::Webp)
        );
        assert_eq!(detect_format(b"RIFF\x04\x00\x00\x00WAVE"), None);
        assert_eq!(detect_format(b"not an image"), None);
        assert_eq!(detect_format(&[]), None);
    }

    #[test]
    fn unsupported_formats_return_errors() {
        let gif = decode(b"GIF89a\x01\x00\x01\x00").expect_err("GIF is not implemented");
        assert!(gif.message.contains("GIF"));
        let jpeg = decode(&[0xff, 0xd8, 0xff, 0xe0, 0x00]).expect_err("JPEG is not implemented");
        assert!(jpeg.message.contains("JPEG"));
        let webp = decode(b"RIFF\x04\x00\x00\x00WEBPVP8 ").expect_err("WebP is not implemented");
        assert!(webp.message.contains("WebP"));
    }

    #[test]
    fn from_rgba_validates_length() {
        let image = ImageData::from_rgba(2, 2, vec![0; 16]).expect("valid buffer");
        assert_eq!((image.width, image.height), (2, 2));
        assert!(ImageData::from_rgba(2, 2, vec![0; 15]).is_err());
        assert!(ImageData::from_rgba(u32::MAX, u32::MAX, vec![0; 4]).is_err());
    }

    #[test]
    fn random_input_never_panics() {
        let mut state = 0x1234_5678_9abc_def0_u64;
        for iteration in 0..200_u32 {
            let mut next = || {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) as u8
            };
            let length = (iteration as usize * 7) % 300;
            let mut bytes = Vec::with_capacity(length);
            for _ in 0..length {
                bytes.push(next());
            }
            let _ = decode(&bytes);
            let _ = decode_png(&bytes);
            let _ = detect_format(&bytes);

            // Mutate a valid PNG too, so the decoders see plausible structure.
            let mut file = png::encode(4, 4, &[0x55; 64]);
            for _ in 0..4 {
                let index = (next() as usize) % file.len();
                file[index] ^= next();
            }
            let _ = decode(&file);
        }
    }
}
