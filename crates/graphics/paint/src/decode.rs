//! Dependency-free raster image decoding into [`ImageData`].
//!
//! The engine renders `<img>` content by decoding bytes here and handing the
//! resulting RGBA to the rasterizer. No external crates are used, so the
//! decoders are deliberately small and strict.
//!
//! # Supported formats
//!
//! * **PNG** — colour types 0 (grey), 2 (RGB), 3 (palette), 4 (grey + alpha)
//!   and 6 (RGBA), at the bit depths the specification allows for each:
//!   1/2/4/8 for types 0 and 3, and 8 for types 2, 4 and 6. Both
//!   non-interlaced and interlaced (Adam7) images are supported; every pass
//!   carries its own filters and is placed into the final RGBA canvas.
//!   `PLTE` and `tRNS` are honoured for palette images, and `tRNS` applies to
//!   sub-byte palette images too. The inflate implementation covers stored,
//!   fixed-Huffman and dynamic-Huffman DEFLATE blocks inside a zlib wrapper
//!   (no preset dictionaries). CRC32 is checked for every critical chunk.
//!   16-bit samples are rejected with a [`DecodeError`], never silently
//!   mis-decoded. Ancillary colour chunks (`iCCP`, `gAMA`, `cHRM`) are ignored;
//!   APNG frames are ignored and the first (default) image is returned.
//! * **GIF** — GIF87a and GIF89a. Logical screen, global and local colour
//!   tables, image descriptors, LZW decompression (clear/end-of-information
//!   codes and code-size growth up to 12 bits), interlacing, and the
//!   transparency index from the Graphic Control Extension are supported.
//!   The first frame is decoded; later frames, and animation timing, are
//!   ignored (there is no compositing timeline in this crate). The canvas is
//!   filled with the logical-screen background colour, or left transparent
//!   when the frame declares a transparent index or has no global table.
//! * **BMP** — uncompressed (`BI_RGB`) 24-bit and 32-bit images, both
//!   bottom-up (positive height) and top-down (negative height). For 32-bit
//!   images the stored fourth byte is used as alpha; if every alpha byte is
//!   zero the image is treated as fully opaque, matching the common practice of
//!   BMP writers that leave the padding byte unset. Other bit depths and
//!   compressed BMP variants are reported as unsupported.
//! * **PPM / PGM** — the binary Netpbm formats `P6` (RGB) and `P5` (grey),
//!   including header comments, 1- and 2-byte samples (`maxval` up to 65535)
//!   and scaling to RGBA8. The ASCII variants (`P1`–`P4`) and `PAM` (`P7`) are
//!   not supported.
//! * **ICO** — Windows icon files. The first PNG-encoded directory entry is
//!   decoded by the PNG decoder above; icon entries that contain a DIB instead
//!   of a PNG are reported as unsupported.
//! * **SVG** — XML vector images are rasterized by [`crate::svg`] into RGBA8,
//!   including gradients and patterns referenced with `url(#id)`.
//! * **JPEG** — baseline sequential DCT JPEG (SOF0), in greyscale and colour.
//!   8-bit samples, 4:4:4 / 4:2:2 / 4:2:0 (and other legal 1..=4 sampling
//!   factor combinations) with nearest-neighbour chroma upsampling, Huffman
//!   entropy coding, restart intervals and 8- or 16-bit quantization tables
//!   are supported. Progressive (SOF2), extended-sequential (SOF1), lossless,
//!   arithmetic-coded and 12-bit JPEGs are reported as unsupported.
//!
//! # Unsupported formats
//!
//! WebP and AVIF are detected and reported as unsupported with a
//! [`DecodeError`], never silently decoded as garbage. Progressive and other
//! non-baseline JPEG modes are likewise reported as unsupported. Animated GIF
//! frames after the first, APNG animation, and 16-bit PNG samples are not
//! decoded. [`detect_format`] lets callers choose a fallback without attempting
//! a decode.

use std::fmt;

use crate::images::ImageData;

/// Largest accepted image edge, in pixels.
///
/// Anything larger is rejected before allocating so a hostile file cannot
/// exhaust memory. Kept identical to the constructor cap enforced by
/// [`ImageData::from_rgba`] so decoders fail early instead of building a large
/// intermediate buffer that would be rejected at the end anyway.
const MAX_DIMENSION: u32 = crate::images::MAX_IMAGE_DIMENSION;

/// Largest accepted pixel count, in pixels.
///
/// A second line of defence on total area before anything is allocated. With a
/// 4096-pixel edge cap the largest possible image is 16.7M pixels (64 MiB), so
/// this bound is generous but cheap to check.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

fn check_pixel_count(width: u32, height: u32, format: &str) -> Result<(), DecodeError> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(DecodeError::new(format!(
            "{format} dimensions {width}x{height} exceed the {MAX_PIXELS} pixel limit"
        )));
    }
    Ok(())
}

/// The image container formats this module can recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG (Portable Network Graphics).
    Png,
    /// GIF (Graphics Interchange Format).
    Gif,
    /// BMP (Windows bitmap).
    Bmp,
    /// PPM (`P6`) or PGM (`P5`) binary Netpbm image.
    Pnm,
    /// ICO (Windows icon) container.
    Ico,
    /// JPEG (JFIF/EXIF).
    Jpeg,
    /// WebP (RIFF container).
    Webp,
    /// AVIF (ISO BMFF container).
    Avif,
    /// SVG (XML vector image).
    Svg,
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

/// Whether `bytes` looks like an SVG document.
///
/// SVG has no single magic number: a document may start with a UTF-8 BOM, an
/// XML declaration, comments and whitespace before its root `<svg>` element.
/// Only a short prefix is inspected so arbitrary text is not scanned wholesale.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    if rest.starts_with(&[0xef, 0xbb, 0xbf]) {
        rest = &rest[3..];
    }
    let prefix_end = rest.len().min(1024);
    rest = &rest[..prefix_end];
    loop {
        rest = rest.trim_ascii_start();
        if let Some(after) = rest.strip_prefix(b"<?xml") {
            // Skip to the end of the XML declaration, if it is closed.
            match after.windows(2).position(|window| window == b"?>") {
                Some(index) => {
                    rest = &after[index + 2..];
                    continue;
                }
                None => return false,
            }
        }
        if let Some(after) = rest.strip_prefix(b"<!--") {
            match after.windows(3).position(|window| window == b"-->") {
                Some(index) => {
                    rest = &after[index + 3..];
                    continue;
                }
                None => return false,
            }
        }
        return rest.starts_with(b"<svg");
    }
}

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
    // Binary Netpbm images: "P5" (PGM) and "P6" (PPM), followed by whitespace.
    if bytes.len() >= 3
        && bytes[0] == b'P'
        && (bytes[1] == b'5' || bytes[1] == b'6')
        && bytes[2].is_ascii_whitespace()
    {
        return Some(ImageFormat::Pnm);
    }
    // ICO: reserved = 0, type = 1 (icon). CUR images use type = 2 and are not
    // recognised as images here.
    if bytes.len() >= 4 && bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some(ImageFormat::Ico);
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(ImageFormat::Webp);
    }
    // AVIF is an ISO BMFF file whose `ftyp` box carries an `avif`/`avis` brand.
    if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis")
    {
        return Some(ImageFormat::Avif);
    }
    if looks_like_svg(bytes) {
        return Some(ImageFormat::Svg);
    }
    None
}

/// Decodes an image, dispatching on the magic bytes.
pub fn decode(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    match detect_format(bytes) {
        Some(ImageFormat::Png) => decode_png(bytes),
        Some(ImageFormat::Bmp) => decode_bmp(bytes),
        Some(ImageFormat::Gif) => decode_gif(bytes),
        Some(ImageFormat::Pnm) => decode_pnm(bytes),
        Some(ImageFormat::Ico) => decode_ico(bytes),
        Some(ImageFormat::Jpeg) => decode_jpeg(bytes),
        Some(ImageFormat::Webp) => Err(DecodeError::unsupported(
            "WebP (VP8/VP8L decoding is not implemented)",
        )),
        Some(ImageFormat::Avif) => Err(DecodeError::unsupported(
            "AVIF (AV1 decoding is not implemented)",
        )),
        Some(ImageFormat::Svg) => decode_svg(bytes),
        None => Err(DecodeError::new("unrecognised image format")),
    }
}

/// Rasterizes an SVG payload (a `.svg` file or a sniffed SVG data URL).
///
/// Both the magic-byte dispatcher above and callers that already know the
/// payload is SVG can use this entry point. Rendering failures become a
/// [`DecodeError`] just like any other decoder failure.
pub fn decode_svg(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    let buffer = crate::svg::rasterize_bytes(bytes)?;
    ImageData::from_rgba(buffer.width, buffer.height, buffer.pixels)
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

/// One interlace pass: the position and spacing of its samples in the final
/// image. A non-interlaced image is a single `(0, 0, 1, 1)` pass.
#[derive(Clone, Copy)]
struct PngPass {
    x_start: usize,
    y_start: usize,
    x_step: usize,
    y_step: usize,
}

/// The seven Adam7 passes, in the order their data appears in the stream.
const ADAM7_PASSES: [PngPass; 7] = [
    PngPass {
        x_start: 0,
        y_start: 0,
        x_step: 8,
        y_step: 8,
    },
    PngPass {
        x_start: 4,
        y_start: 0,
        x_step: 8,
        y_step: 8,
    },
    PngPass {
        x_start: 0,
        y_start: 4,
        x_step: 4,
        y_step: 8,
    },
    PngPass {
        x_start: 2,
        y_start: 0,
        x_step: 4,
        y_step: 4,
    },
    PngPass {
        x_start: 0,
        y_start: 2,
        x_step: 2,
        y_step: 4,
    },
    PngPass {
        x_start: 1,
        y_start: 0,
        x_step: 2,
        y_step: 2,
    },
    PngPass {
        x_start: 0,
        y_start: 1,
        x_step: 1,
        y_step: 2,
    },
];

const NON_INTERLACED_PASS: [PngPass; 1] = [PngPass {
    x_start: 0,
    y_start: 0,
    x_step: 1,
    y_step: 1,
}];

/// Number of pixels a pass covers along one axis of the final image.
fn pass_extent(full: usize, start: usize, step: usize) -> usize {
    if full <= start {
        0
    } else {
        (full - start).div_ceil(step)
    }
}

/// Row size in bytes for `width` pixels of the given sample layout.
fn png_row_bytes(width: usize, channels: usize, bit_depth: u8) -> Result<usize, DecodeError> {
    let bits = width
        .checked_mul(channels)
        .and_then(|value| value.checked_mul(usize::from(bit_depth)))
        .ok_or_else(|| DecodeError::new("PNG row size overflow"))?;
    Ok(bits.div_ceil(8))
}

/// Bytes per pixel used by the PNG row filters (always at least one).
fn png_filter_bpp(channels: usize, bit_depth: u8) -> usize {
    (channels * usize::from(bit_depth)).div_ceil(8).max(1)
}

/// Maps a colour type and bit depth to a channel count, rejecting layouts the
/// specification does not allow.
fn png_channels(color_type: u8, bit_depth: u8) -> Result<usize, DecodeError> {
    match color_type {
        0 => match bit_depth {
            1 | 2 | 4 | 8 => Ok(1),
            16 => Err(DecodeError::unsupported("16-bit PNG samples")),
            other => Err(DecodeError::new(format!(
                "invalid PNG bit depth {other} for greyscale"
            ))),
        },
        2 => match bit_depth {
            8 => Ok(3),
            16 => Err(DecodeError::unsupported("16-bit PNG samples")),
            other => Err(DecodeError::new(format!(
                "invalid PNG bit depth {other} for RGB"
            ))),
        },
        3 => match bit_depth {
            1 | 2 | 4 | 8 => Ok(1),
            16 => Err(DecodeError::unsupported("16-bit PNG samples")),
            other => Err(DecodeError::new(format!(
                "invalid PNG bit depth {other} for palette"
            ))),
        },
        4 => match bit_depth {
            8 => Ok(2),
            16 => Err(DecodeError::unsupported("16-bit PNG samples")),
            other => Err(DecodeError::new(format!(
                "invalid PNG bit depth {other} for grey + alpha"
            ))),
        },
        6 => match bit_depth {
            8 => Ok(4),
            16 => Err(DecodeError::unsupported("16-bit PNG samples")),
            other => Err(DecodeError::new(format!(
                "invalid PNG bit depth {other} for RGBA"
            ))),
        },
        other => Err(DecodeError::unsupported(format!("PNG colour type {other}"))),
    }
}

/// Reads packed sample number `index` (1, 2 and 4 bits are packed MSB first).
fn png_sample(row: &[u8], index: usize, bit_depth: u8) -> Result<u8, DecodeError> {
    match bit_depth {
        1 | 2 | 4 => {
            let per_byte = 8 / usize::from(bit_depth);
            let byte = *row
                .get(index / per_byte)
                .ok_or_else(|| DecodeError::new("PNG sample index out of range"))?;
            let shift = (per_byte - 1 - index % per_byte) * usize::from(bit_depth);
            Ok((byte >> shift) & ((1_u8 << bit_depth) - 1))
        }
        8 => row
            .get(index)
            .copied()
            .ok_or_else(|| DecodeError::new("PNG sample index out of range")),
        other => Err(DecodeError::new(format!(
            "unsupported PNG sample bit depth {other}"
        ))),
    }
}

/// Converts one sample position of an unfiltered row to RGBA.
fn png_pixel(
    row: &[u8],
    index: usize,
    color_type: u8,
    bit_depth: u8,
    palette: Option<&[[u8; 3]]>,
    transparency: &[u8],
) -> Result<[u8; 4], DecodeError> {
    match color_type {
        0 => {
            let sample = png_sample(row, index, bit_depth)?;
            let scale = match bit_depth {
                1 => 255,
                2 => 85,
                4 => 17,
                _ => 1,
            };
            let grey = sample * scale;
            Ok([grey, grey, grey, 255])
        }
        2 => {
            let start = index * 3;
            let pixel = row
                .get(start..start + 3)
                .ok_or_else(|| DecodeError::new("PNG RGB sample out of range"))?;
            Ok([pixel[0], pixel[1], pixel[2], 255])
        }
        3 => {
            let entry_index = usize::from(png_sample(row, index, bit_depth)?);
            let table =
                palette.ok_or_else(|| DecodeError::new("palette PNG is missing its PLTE chunk"))?;
            let entry = table
                .get(entry_index)
                .ok_or_else(|| DecodeError::new("palette index is outside the PLTE chunk"))?;
            let alpha = transparency.get(entry_index).copied().unwrap_or(255);
            Ok([entry[0], entry[1], entry[2], alpha])
        }
        4 => {
            let start = index * 2;
            let pixel = row
                .get(start..start + 2)
                .ok_or_else(|| DecodeError::new("PNG grey + alpha sample out of range"))?;
            Ok([pixel[0], pixel[0], pixel[0], pixel[1]])
        }
        _ => {
            let start = index * 4;
            let pixel = row
                .get(start..start + 4)
                .ok_or_else(|| DecodeError::new("PNG RGBA sample out of range"))?;
            Ok([pixel[0], pixel[1], pixel[2], pixel[3]])
        }
    }
}

/// Decodes a PNG image into RGBA8.
///
/// Both non-interlaced and Adam7-interlaced images are supported for colour
/// types 0, 2, 3, 4 and 6 at the bit depths listed in the module docs.
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
    if header.interlace > 1 {
        return Err(DecodeError::unsupported("PNG interlace method"));
    }
    let channels = png_channels(header.color_type, header.bit_depth)?;
    if header.color_type == 3 && palette.is_none() {
        return Err(DecodeError::new("palette PNG is missing its PLTE chunk"));
    }

    let width = header.width as usize;
    let height = header.height as usize;
    check_pixel_count(header.width, header.height, "PNG")?;
    let passes: &[PngPass] = if header.interlace == 0 {
        &NON_INTERLACED_PASS
    } else {
        &ADAM7_PASSES
    };

    // Each pass has its own filter byte, rows and (crucially) its own
    // previous-row history, so the total decompressed size is the sum over
    // passes rather than one stride times the full height.
    let mut raw_length = 0_usize;
    for pass in passes {
        let pass_width = pass_extent(width, pass.x_start, pass.x_step);
        let pass_height = pass_extent(height, pass.y_start, pass.y_step);
        if pass_width == 0 || pass_height == 0 {
            continue;
        }
        let row_bytes = png_row_bytes(pass_width, channels, header.bit_depth)?;
        let pass_length = row_bytes
            .checked_add(1)
            .and_then(|row| row.checked_mul(pass_height))
            .ok_or_else(|| DecodeError::new("PNG image data size overflow"))?;
        raw_length = raw_length
            .checked_add(pass_length)
            .ok_or_else(|| DecodeError::new("PNG image data size overflow"))?;
    }

    let raw = inflate_zlib(&compressed, raw_length)?;
    if raw.len() != raw_length {
        return Err(DecodeError::new(
            "PNG image data does not match its declared size",
        ));
    }

    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| DecodeError::new("PNG image size overflow"))?;
    let mut pixels = vec![
        0_u8;
        pixel_count
            .checked_mul(4)
            .ok_or_else(|| DecodeError::new("PNG image size overflow"))?
    ];
    let filter_bpp = png_filter_bpp(channels, header.bit_depth);
    let palette_table = palette.as_deref();
    let mut offset = 0_usize;

    for pass in passes {
        let pass_width = pass_extent(width, pass.x_start, pass.x_step);
        let pass_height = pass_extent(height, pass.y_start, pass.y_step);
        if pass_width == 0 || pass_height == 0 {
            continue;
        }
        let row_bytes = png_row_bytes(pass_width, channels, header.bit_depth)?;
        let mut previous = vec![0_u8; row_bytes];
        let mut current = vec![0_u8; row_bytes];
        for row in 0..pass_height {
            let filter = *raw
                .get(offset)
                .ok_or_else(|| DecodeError::new("truncated PNG image data"))?;
            let row_start = offset + 1;
            let row_data = raw
                .get(row_start..row_start + row_bytes)
                .ok_or_else(|| DecodeError::new("truncated PNG image data"))?;
            current.copy_from_slice(row_data);
            offset = row_start + row_bytes;
            unfilter(filter, &mut current, &previous, filter_bpp)?;

            let y = pass.y_start + row * pass.y_step;
            for column in 0..pass_width {
                let x = pass.x_start + column * pass.x_step;
                let color = png_pixel(
                    &current,
                    column,
                    header.color_type,
                    header.bit_depth,
                    palette_table,
                    &transparency,
                )?;
                let index = (y * width + x) * 4;
                pixels[index..index + 4].copy_from_slice(&color);
            }
            std::mem::swap(&mut previous, &mut current);
        }
    }

    if offset != raw_length {
        return Err(DecodeError::new(
            "PNG image data does not match its declared size",
        ));
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
// GIF
// ---------------------------------------------------------------------------

const GIF_SIGNATURE_87A: &[u8; 6] = b"GIF87a";
const GIF_SIGNATURE_89A: &[u8; 6] = b"GIF89a";

/// Reads a little-endian `u16` from a GIF byte stream.
fn gif_u16(bytes: &[u8], offset: usize) -> Result<u16, DecodeError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| DecodeError::new("truncated GIF data"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

/// Reads a colour table of `entries` RGB triples.
fn gif_color_table(
    bytes: &[u8],
    offset: usize,
    entries: usize,
) -> Result<(Vec<[u8; 3]>, usize), DecodeError> {
    let length = entries
        .checked_mul(3)
        .ok_or_else(|| DecodeError::new("GIF colour table size overflow"))?;
    let table = bytes
        .get(offset..offset + length)
        .ok_or_else(|| DecodeError::new("truncated GIF colour table"))?;
    let entries: Vec<[u8; 3]> = table.as_chunks::<3>().0.to_vec();
    Ok((entries, offset + length))
}

/// Skips a chain of GIF data sub-blocks, returning the offset after the
/// terminating zero-length block.
fn gif_skip_sub_blocks(bytes: &[u8], mut offset: usize) -> Result<usize, DecodeError> {
    loop {
        let length = usize::from(
            *bytes
                .get(offset)
                .ok_or_else(|| DecodeError::new("truncated GIF sub-block"))?,
        );
        offset += 1;
        if length == 0 {
            return Ok(offset);
        }
        offset = offset
            .checked_add(length)
            .ok_or_else(|| DecodeError::new("GIF sub-block size overflow"))?;
        if offset > bytes.len() {
            return Err(DecodeError::new("truncated GIF sub-block data"));
        }
    }
}

/// Concatenates a chain of GIF data sub-blocks into one byte buffer.
fn gif_read_sub_blocks(bytes: &[u8], mut offset: usize) -> Result<(Vec<u8>, usize), DecodeError> {
    let mut data = Vec::new();
    loop {
        let length = usize::from(
            *bytes
                .get(offset)
                .ok_or_else(|| DecodeError::new("truncated GIF sub-block"))?,
        );
        offset += 1;
        if length == 0 {
            return Ok((data, offset));
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| DecodeError::new("GIF sub-block size overflow"))?;
        let block = bytes
            .get(offset..end)
            .ok_or_else(|| DecodeError::new("truncated GIF sub-block data"))?;
        if data.len().saturating_add(block.len()) > MAX_PIXELS as usize + 16 {
            return Err(DecodeError::new("GIF LZW data is too large"));
        }
        data.extend_from_slice(block);
        offset = end;
    }
}

/// Bit reader for GIF LZW data. Codes are packed least-significant bit first,
/// with no padding between sub-blocks (the sub-block framing is removed before
/// this reader sees the bytes).
struct GifBitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> GifBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn read(&mut self, count: u32) -> Result<u32, DecodeError> {
        let mut value = 0_u32;
        for index in 0..count {
            let byte = *self
                .data
                .get(self.bit / 8)
                .ok_or_else(|| DecodeError::new("truncated GIF LZW data"))?;
            let bit = (byte >> (self.bit % 8)) & 1;
            value |= u32::from(bit) << index;
            self.bit += 1;
        }
        Ok(value)
    }
}

/// Decompresses GIF LZW data into exactly `expected` colour indices.
///
/// `min_code_size` is the value stored in the image descriptor (normally 2..=8;
/// 1 is accepted for two-colour images). The dictionary is reset whenever a
/// clear code appears, and code width grows from `min_code_size + 1` up to 12
/// bits as entries are added.
fn gif_lzw_decode(data: &[u8], min_code_size: u8, expected: usize) -> Result<Vec<u8>, DecodeError> {
    if !(1..=8).contains(&min_code_size) {
        return Err(DecodeError::new(format!(
            "invalid GIF LZW minimum code size {min_code_size}"
        )));
    }
    let clear = 1_u32 << min_code_size;
    let end_of_information = clear + 1;
    let initial_code_size = u32::from(min_code_size) + 1;

    // 12-bit codes address at most 4096 entries. Literal entries are
    // pre-populated so `previous` lookups never need a special case.
    let mut dictionary: Vec<Vec<u8>> = vec![Vec::new(); 4096];
    for (index, entry) in dictionary.iter_mut().take(clear as usize).enumerate() {
        entry.push(index as u8);
    }

    let mut reader = GifBitReader::new(data);
    let mut out = Vec::with_capacity(expected.min(MAX_PIXELS as usize));
    let mut code_size = initial_code_size;
    let mut next_code = end_of_information + 1;
    let mut previous: Option<u32> = None;

    loop {
        let code = reader.read(code_size)?;
        if code == clear {
            code_size = initial_code_size;
            next_code = end_of_information + 1;
            previous = None;
            continue;
        }
        if code == end_of_information {
            break;
        }

        let entry: Vec<u8> = if code < clear {
            vec![code as u8]
        } else if code < next_code {
            dictionary
                .get(code as usize)
                .filter(|entry| !entry.is_empty())
                .cloned()
                .ok_or_else(|| DecodeError::new("invalid GIF LZW code"))?
        } else if code == next_code {
            // The code being read is the one just about to be added: the
            // decoder emits previous + previous[0] ("KwKwK").
            let prior = previous
                .and_then(|prior| dictionary.get(prior as usize))
                .filter(|entry| !entry.is_empty())
                .ok_or_else(|| DecodeError::new("invalid GIF LZW code"))?;
            let mut entry = prior.clone();
            entry.push(prior[0]);
            entry
        } else {
            return Err(DecodeError::new("invalid GIF LZW code"));
        };

        if out.len().saturating_add(entry.len()) > expected {
            return Err(DecodeError::new(
                "GIF LZW data expands past the frame's pixel count",
            ));
        }
        out.extend_from_slice(&entry);

        if let Some(prior) = previous
            && (next_code as usize) < dictionary.len()
        {
            let mut new_entry = dictionary[prior as usize].clone();
            new_entry.push(entry[0]);
            dictionary[next_code as usize] = new_entry;
            next_code += 1;
            if next_code == (1_u32 << code_size) && code_size < 12 {
                code_size += 1;
            }
        }
        previous = Some(code);

        if out.len() == expected {
            break;
        }
    }

    if out.len() != expected {
        return Err(DecodeError::new("GIF LZW data is truncated"));
    }
    Ok(out)
}

/// Row order for an interlaced GIF frame: four passes, each picking every
/// eighth, eighth, fourth and second row respectively.
fn gif_interlaced_rows(height: usize) -> Vec<usize> {
    let mut rows = Vec::with_capacity(height);
    for (start, step) in [(0_usize, 8_usize), (4, 8), (2, 4), (1, 2)] {
        let mut row = start;
        while row < height {
            rows.push(row);
            row += step;
        }
    }
    rows
}

/// Decodes the first frame of a GIF image into RGBA8.
///
/// Animated GIFs are accepted but only their first frame is returned; there is
/// no frame-compositing model in this crate. Malformed files produce a
/// [`DecodeError`] rather than a panic.
fn decode_gif(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    if bytes.len() < 13 {
        return Err(DecodeError::new("truncated GIF header"));
    }
    if !bytes.starts_with(GIF_SIGNATURE_87A) && !bytes.starts_with(GIF_SIGNATURE_89A) {
        return Err(DecodeError::new("not a GIF file"));
    }

    let width = u32::from(gif_u16(bytes, 6)?);
    let height = u32::from(gif_u16(bytes, 8)?);
    if width == 0 || height == 0 {
        return Err(DecodeError::new("GIF has a zero dimension"));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(DecodeError::new(format!(
            "GIF dimensions {width}x{height} exceed the {MAX_DIMENSION} pixel limit"
        )));
    }
    check_pixel_count(width, height, "GIF")?;

    let packed = bytes[10];
    let background_index = bytes[11];
    let mut offset = 13_usize;

    let global_table = if packed & 0x80 != 0 {
        let entries = 1_usize << (usize::from(packed & 0x07) + 1);
        let (table, next) = gif_color_table(bytes, offset, entries)?;
        offset = next;
        Some(table)
    } else {
        None
    };

    let mut transparent_index: Option<u8> = None;

    loop {
        let block = *bytes
            .get(offset)
            .ok_or_else(|| DecodeError::new("truncated GIF data (missing trailer)"))?;
        offset += 1;
        match block {
            0x21 => {
                // Extension introducer, followed by a label and sub-blocks.
                let label = *bytes
                    .get(offset)
                    .ok_or_else(|| DecodeError::new("truncated GIF extension"))?;
                offset += 1;
                if label == 0xf9 {
                    // Graphic Control Extension: size (4), flags, delay,
                    // transparent index, terminator.
                    let size = usize::from(
                        *bytes
                            .get(offset)
                            .ok_or_else(|| DecodeError::new("truncated GIF control block"))?,
                    );
                    if size < 4 {
                        return Err(DecodeError::new(
                            "invalid GIF Graphic Control Extension size",
                        ));
                    }
                    let payload = bytes
                        .get(offset + 1..offset + 1 + size)
                        .ok_or_else(|| DecodeError::new("truncated GIF control block"))?;
                    let flags = payload[0];
                    transparent_index = if flags & 0x01 != 0 {
                        Some(payload[3])
                    } else {
                        None
                    };
                    offset = gif_skip_sub_blocks(bytes, offset + 1 + size)?;
                } else {
                    offset = gif_skip_sub_blocks(bytes, offset)?;
                }
            }
            0x2c => {
                // Image descriptor: left, top, width, height, packed flags.
                let left = usize::from(gif_u16(bytes, offset)?);
                let top = usize::from(gif_u16(bytes, offset + 2)?);
                let frame_width = usize::from(gif_u16(bytes, offset + 4)?);
                let frame_height = usize::from(gif_u16(bytes, offset + 6)?);
                let frame_packed = *bytes
                    .get(offset + 8)
                    .ok_or_else(|| DecodeError::new("truncated GIF image descriptor"))?;
                offset += 9;

                if frame_width == 0 || frame_height == 0 {
                    return Err(DecodeError::new("GIF frame has a zero dimension"));
                }
                if frame_width > MAX_DIMENSION as usize || frame_height > MAX_DIMENSION as usize {
                    return Err(DecodeError::new(format!(
                        "GIF frame dimensions {frame_width}x{frame_height} exceed the {MAX_DIMENSION} pixel limit"
                    )));
                }
                let frame_pixels = frame_width
                    .checked_mul(frame_height)
                    .ok_or_else(|| DecodeError::new("GIF frame size overflow"))?;
                if frame_pixels as u64 > MAX_PIXELS {
                    return Err(DecodeError::new(format!(
                        "GIF frame pixel count exceeds the {MAX_PIXELS} pixel limit"
                    )));
                }

                let local_table = if frame_packed & 0x80 != 0 {
                    let entries = 1_usize << (usize::from(frame_packed & 0x07) + 1);
                    let (table, next) = gif_color_table(bytes, offset, entries)?;
                    offset = next;
                    Some(table)
                } else {
                    None
                };
                let interlaced = frame_packed & 0x40 != 0;

                let color_table = local_table.as_ref().or(global_table.as_ref());
                let color_table = color_table.ok_or_else(|| {
                    DecodeError::new("GIF frame has no global or local colour table")
                })?;

                let min_code_size = *bytes
                    .get(offset)
                    .ok_or_else(|| DecodeError::new("truncated GIF LZW header"))?;
                offset += 1;
                let (lzw_data, _next) = gif_read_sub_blocks(bytes, offset)?;

                let indices = gif_lzw_decode(&lzw_data, min_code_size, frame_pixels)?;

                // Fill the canvas with the logical-screen background, unless a
                // transparent index is in effect, in which case anything not
                // covered by the frame must stay transparent.
                let canvas_pixels = (width as usize)
                    .checked_mul(height as usize)
                    .ok_or_else(|| DecodeError::new("GIF canvas size overflow"))?;
                let mut pixels = vec![0_u8; canvas_pixels * 4];
                if transparent_index.is_none()
                    && let Some(color) = global_table
                        .as_ref()
                        .and_then(|table| table.get(usize::from(background_index)))
                {
                    for pixel in pixels.as_chunks_mut::<4>().0 {
                        pixel.copy_from_slice(&[color[0], color[1], color[2], 255]);
                    }
                }

                let row_order = if interlaced {
                    gif_interlaced_rows(frame_height)
                } else {
                    (0..frame_height).collect()
                };

                let canvas_width = width as usize;
                let canvas_height = height as usize;
                for (source_row, row) in row_order.into_iter().enumerate() {
                    let start = source_row * frame_width;
                    let row_indices = indices
                        .get(start..start + frame_width)
                        .ok_or_else(|| DecodeError::new("truncated GIF frame data"))?;
                    let y = top + row;
                    if y >= canvas_height {
                        continue;
                    }
                    for (column, &index) in row_indices.iter().enumerate() {
                        if Some(index) == transparent_index {
                            continue;
                        }
                        let x = left + column;
                        if x >= canvas_width {
                            continue;
                        }
                        let entry = color_table.get(usize::from(index)).ok_or_else(|| {
                            DecodeError::new("GIF colour index is outside the colour table")
                        })?;
                        let target = (y * canvas_width + x) * 4;
                        pixels[target..target + 4]
                            .copy_from_slice(&[entry[0], entry[1], entry[2], 255]);
                    }
                }

                return ImageData::from_rgba(width, height, pixels);
            }
            0x3b => {
                return Err(DecodeError::new("GIF contains no image frame"));
            }
            0x00 => {
                // Some encoders pad between blocks; skip the filler byte.
                continue;
            }
            other => {
                return Err(DecodeError::new(format!(
                    "unexpected GIF block 0x{other:02x}"
                )));
            }
        }
    }
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

// ---------------------------------------------------------------------------
// PPM / PGM (binary Netpbm, P5 and P6)
// ---------------------------------------------------------------------------

/// Reads the whitespace-separated numeric tokens of a Netpbm header.
struct PnmHeaderReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PnmHeaderReader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    /// Skips ASCII whitespace and `#` comments, which run to the end of a line.
    fn skip_space(&mut self) {
        while let Some(&byte) = self.data.get(self.pos) {
            match byte {
                _ if byte.is_ascii_whitespace() => self.pos += 1,
                b'#' => {
                    while let Some(&comment_byte) = self.data.get(self.pos) {
                        if comment_byte == b'\n' || comment_byte == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// Reads one unsigned decimal token, rejecting missing or non-numeric data.
    fn read_number(&mut self, name: &str) -> Result<u32, DecodeError> {
        self.skip_space();
        let start = self.pos;
        while self
            .data
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(DecodeError::new(format!("missing PNM {name} value")));
        }
        let token = std::str::from_utf8(&self.data[start..self.pos])
            .map_err(|_| DecodeError::new(format!("invalid PNM {name} value")))?;
        token
            .parse::<u32>()
            .map_err(|_| DecodeError::new(format!("invalid PNM {name} value")))
    }

    /// Consumes the single whitespace byte separating the header from the
    /// raster. Netpbm permits comments only before that byte, so exactly one is
    /// consumed: the raster itself may legitimately contain whitespace bytes.
    fn finish_header(&mut self) -> Result<usize, DecodeError> {
        let byte = *self
            .data
            .get(self.pos)
            .ok_or_else(|| DecodeError::new("missing PNM raster data"))?;
        if !byte.is_ascii_whitespace() {
            return Err(DecodeError::new("missing whitespace after PNM header"));
        }
        self.pos += 1;
        Ok(self.pos)
    }
}

/// Decodes a binary Netpbm image (`P5` greyscale or `P6` RGB) into RGBA8.
fn decode_pnm(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    if bytes.len() < 3 || bytes[0] != b'P' || (bytes[1] != b'5' && bytes[1] != b'6') {
        return Err(DecodeError::new("not a binary PPM/PGM file"));
    }
    let colour = bytes[1] == b'6';
    let channels = if colour { 3_usize } else { 1 };

    let mut reader = PnmHeaderReader::new(bytes, 2);
    let width = reader.read_number("width")?;
    let height = reader.read_number("height")?;
    let max_value = reader.read_number("maxval")?;

    if width == 0 || height == 0 {
        return Err(DecodeError::new("PNM has a zero dimension"));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(DecodeError::new(format!(
            "PNM dimensions {width}x{height} exceed the {MAX_DIMENSION} pixel limit"
        )));
    }
    check_pixel_count(width, height, "PNM")?;
    if max_value == 0 || max_value > 65535 {
        return Err(DecodeError::new(format!("invalid PNM maxval {max_value}")));
    }

    let raster_start = reader.finish_header()?;
    let sample_bytes = if max_value > 255 { 2_usize } else { 1 };
    let width_usize = width as usize;
    let height_usize = height as usize;
    let pixel_count = width_usize
        .checked_mul(height_usize)
        .ok_or_else(|| DecodeError::new("PNM image size overflow"))?;
    let raster_len = pixel_count
        .checked_mul(channels)
        .and_then(|samples| samples.checked_mul(sample_bytes))
        .ok_or_else(|| DecodeError::new("PNM raster size overflow"))?;
    let raster_end = raster_start
        .checked_add(raster_len)
        .ok_or_else(|| DecodeError::new("PNM raster size overflow"))?;
    if raster_end > bytes.len() {
        return Err(DecodeError::new("truncated PNM raster data"));
    }
    let raster = &bytes[raster_start..raster_end];

    let scale = |value: u32| -> u8 { ((u64::from(value) * 255) / u64::from(max_value)) as u8 };
    let mut pixels = vec![0_u8; pixel_count * 4];
    for pixel in 0..pixel_count {
        let mut components = [0_u8; 3];
        for (channel, component) in components.iter_mut().enumerate().take(channels) {
            let offset = (pixel * channels + channel) * sample_bytes;
            let value = if sample_bytes == 1 {
                u32::from(raster[offset])
            } else {
                (u32::from(raster[offset]) << 8) | u32::from(raster[offset + 1])
            };
            if value > max_value {
                return Err(DecodeError::new("PNM sample exceeds maxval"));
            }
            *component = scale(value);
        }
        let target = pixel * 4;
        if colour {
            pixels[target..target + 4].copy_from_slice(&[
                components[0],
                components[1],
                components[2],
                255,
            ]);
        } else {
            let grey = components[0];
            pixels[target..target + 4].copy_from_slice(&[grey, grey, grey, 255]);
        }
    }

    ImageData::from_rgba(width, height, pixels)
}

// ---------------------------------------------------------------------------
// ICO
// ---------------------------------------------------------------------------

/// Decodes the first PNG-encoded entry of an ICO (Windows icon) file.
///
/// The icon directory is only a container; the embedded PNG is handed to the
/// PNG decoder above. Entries holding a DIB (BMP) rather than a PNG are reported
/// as unsupported instead of being mis-parsed.
fn decode_ico(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    if bytes.len() < 6 {
        return Err(DecodeError::new("truncated ICO header"));
    }
    if bytes[0..2] != [0, 0] || u16::from_le_bytes([bytes[2], bytes[3]]) != 1 {
        return Err(DecodeError::new("not an ICO file"));
    }
    let count = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
    if count == 0 {
        return Err(DecodeError::new("ICO contains no image entries"));
    }
    let table_end = 6_usize
        .checked_add(
            count
                .checked_mul(16)
                .ok_or_else(|| DecodeError::new("ICO directory size overflow"))?,
        )
        .ok_or_else(|| DecodeError::new("ICO directory size overflow"))?;
    if table_end > bytes.len() {
        return Err(DecodeError::new("truncated ICO directory"));
    }

    let mut saw_dib = false;
    for index in 0..count {
        let entry_start = 6 + index * 16;
        let entry = &bytes[entry_start..entry_start + 16];
        let size = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        if size == 0 || offset == 0 {
            continue;
        }
        let end = offset
            .checked_add(size)
            .ok_or_else(|| DecodeError::new("ICO entry size overflow"))?;
        if end > bytes.len() {
            return Err(DecodeError::new("ICO entry points outside the file"));
        }
        let payload = &bytes[offset..end];
        if payload.starts_with(&PNG_SIGNATURE) {
            return decode_png(payload);
        }
        saw_dib = true;
    }

    if saw_dib {
        Err(DecodeError::unsupported(
            "ICO entries that contain a DIB (BMP) instead of PNG",
        ))
    } else {
        Err(DecodeError::new("ICO contains no decodable image entry"))
    }
}

// ---------------------------------------------------------------------------
// JPEG (baseline sequential DCT)
// ---------------------------------------------------------------------------
//
// A small, dependency-free baseline JPEG decoder. It parses the marker stream
// up to the first scan, Huffman-decodes DC differences and AC run/size pairs,
// dequantizes, performs a separable floating-point inverse DCT and converts
// YCbCr to RGB. Chroma planes are upsampled with nearest-neighbour sampling
// according to their sampling factors, which covers 4:4:4, 4:2:2 and 4:2:0.

/// Zig-zag order: entry `k` is the natural (row-major) index of the `k`-th
/// coefficient in the bit stream. Quantization tables and entropy-coded
/// coefficients both arrive in zig-zag order.
const JPEG_ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

fn jpeg_err(message: impl Into<String>) -> DecodeError {
    DecodeError::new(format!("JPEG: {}", message.into()))
}

fn jpeg_unsupported(message: impl Into<String>) -> DecodeError {
    DecodeError::unsupported(format!("JPEG {}", message.into()))
}

/// Canonical Huffman table for one JPEG DC or AC table (lengths 1..=16).
struct JpegHuffman {
    /// Number of codes of each length; index 0 is unused.
    counts: [u16; 17],
    /// Symbols in canonical (code) order.
    symbols: Vec<u8>,
}

impl JpegHuffman {
    fn new(counts: [u16; 17], symbols: Vec<u8>) -> Result<Self, DecodeError> {
        let total: usize = counts.iter().map(|&count| count as usize).sum();
        if total != symbols.len() {
            return Err(jpeg_err("Huffman table symbol count mismatch"));
        }
        // Reject over-subscribed tables; incomplete tables are legal in JPEG.
        let mut left: i32 = 1;
        for &count in counts.iter().skip(1) {
            left = left * 2 - i32::from(count);
            if left < 0 {
                return Err(jpeg_err("over-subscribed JPEG Huffman table"));
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut JpegBitReader<'_>) -> Result<u8, DecodeError> {
        let mut code = 0_u32;
        let mut first = 0_u32;
        let mut index = 0_usize;
        for length in 1..=16_u32 {
            code = (code << 1) | reader.read_bit()?;
            let count = u32::from(self.counts[length as usize]);
            if code >= first && code - first < count {
                return self
                    .symbols
                    .get(index + (code - first) as usize)
                    .copied()
                    .ok_or_else(|| jpeg_err("invalid JPEG Huffman code"));
            }
            index += count as usize;
            first = (first + count) << 1;
        }
        Err(jpeg_err("invalid JPEG Huffman code"))
    }
}

/// Bit reader for the entropy-coded segment. JPEG packs bits most-significant
/// bit first and byte-stuffs `0xff` as `ff 00`; restart markers are handled
/// explicitly by [`JpegBitReader::restart`].
struct JpegBitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u8,
    bits: u8,
}

impl<'a> JpegBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buffer: 0,
            bits: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u32, DecodeError> {
        if self.bits == 0 {
            self.fill()?;
        }
        self.bits -= 1;
        Ok(u32::from((self.buffer >> self.bits) & 1))
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, DecodeError> {
        let mut value = 0_u32;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }

    fn fill(&mut self) -> Result<(), DecodeError> {
        let byte = *self
            .data
            .get(self.pos)
            .ok_or_else(|| jpeg_err("truncated entropy-coded data"))?;
        self.pos += 1;
        if byte == 0xff {
            let next = *self
                .data
                .get(self.pos)
                .ok_or_else(|| jpeg_err("truncated entropy-coded data"))?;
            if next == 0x00 {
                self.pos += 1; // stuffed data byte
            } else {
                return Err(jpeg_err("unexpected marker in entropy-coded data"));
            }
        }
        self.buffer = byte;
        self.bits = 8;
        Ok(())
    }

    /// Aligns to the next byte, skips fill bytes and consumes restart marker
    /// `expected` (`0xd0..=0xd7`), resetting the buffered bits.
    fn restart(&mut self, expected: u8) -> Result<(), DecodeError> {
        self.buffer = 0;
        self.bits = 0;
        loop {
            let byte = *self
                .data
                .get(self.pos)
                .ok_or_else(|| jpeg_err("missing restart marker"))?;
            self.pos += 1;
            if byte != 0xff {
                return Err(jpeg_err("expected a restart marker"));
            }
            let marker = *self
                .data
                .get(self.pos)
                .ok_or_else(|| jpeg_err("truncated restart marker"))?;
            if marker == 0xff {
                continue; // fill byte; the next iteration consumes it as prefix
            }
            self.pos += 1;
            if marker != expected {
                return Err(jpeg_err(format!(
                    "unexpected restart marker 0x{marker:02x}"
                )));
            }
            return Ok(());
        }
    }
}

/// Extends a `bits`-bit magnitude read from a Huffman-coded stream into a
/// signed coefficient (the "receive and extend" procedure).
fn jpeg_receive_extend(bits: usize, reader: &mut JpegBitReader<'_>) -> Result<i32, DecodeError> {
    if bits == 0 {
        return Ok(0);
    }
    if bits > 16 {
        return Err(jpeg_err("invalid coefficient magnitude"));
    }
    let value = reader.read_bits(bits as u8)? as i32;
    if value < (1_i32 << (bits - 1)) {
        Ok(value - ((1_i32 << bits) - 1))
    } else {
        Ok(value)
    }
}

/// One component of a baseline frame, with the scan's Huffman table selectors
/// filled in once SOS has been parsed.
struct JpegComponent {
    h: u8,
    v: u8,
    quant: usize,
    dc_table: usize,
    ac_table: usize,
}

struct JpegFrame {
    width: u32,
    height: u32,
    components: Vec<JpegComponent>,
    quant: [[i32; 64]; 4],
    restart_interval: u16,
}

/// Reads the next marker byte after the initial `0xff`, tolerating fill bytes.
fn jpeg_next_marker(data: &[u8], pos: &mut usize) -> Result<u8, DecodeError> {
    let prefix = *data.get(*pos).ok_or_else(|| jpeg_err("missing marker"))?;
    *pos += 1;
    if prefix != 0xff {
        return Err(jpeg_err("expected a 0xff marker prefix"));
    }
    loop {
        let marker = *data.get(*pos).ok_or_else(|| jpeg_err("truncated marker"))?;
        *pos += 1;
        if marker != 0xff {
            return Ok(marker);
        }
    }
}

/// Returns a length-prefixed segment's payload and advances past it.
fn jpeg_segment<'a>(data: &'a [u8], pos: &mut usize, name: &str) -> Result<&'a [u8], DecodeError> {
    let header = data
        .get(*pos..*pos + 2)
        .ok_or_else(|| jpeg_err(format!("truncated {name} segment")))?;
    let length = usize::from(u16::from_be_bytes([header[0], header[1]]));
    if length < 2 {
        return Err(jpeg_err(format!("invalid {name} segment length")));
    }
    let start = *pos + 2;
    let end = start
        .checked_add(length - 2)
        .ok_or_else(|| jpeg_err(format!("invalid {name} segment length")))?;
    let payload = data
        .get(start..end)
        .ok_or_else(|| jpeg_err(format!("truncated {name} segment")))?;
    *pos = end;
    Ok(payload)
}

fn jpeg_parse_dqt(data: &[u8], quant: &mut [[i32; 64]; 4]) -> Result<(), DecodeError> {
    let mut pos = 0;
    while pos < data.len() {
        let spec = data[pos];
        pos += 1;
        let precision = spec >> 4;
        let table = usize::from(spec & 0x0f);
        if table >= 4 {
            return Err(jpeg_err("invalid quantization table selector"));
        }
        let bytes_per_value = match precision {
            0 => 1,
            1 => 2,
            _ => return Err(jpeg_err("invalid quantization table precision")),
        };
        let slice = data
            .get(pos..pos + 64 * bytes_per_value)
            .ok_or_else(|| jpeg_err("truncated DQT segment"))?;
        for (zigzag, slot) in JPEG_ZIGZAG.iter().enumerate() {
            let value = if bytes_per_value == 1 {
                i32::from(slice[zigzag])
            } else {
                (i32::from(slice[2 * zigzag]) << 8) | i32::from(slice[2 * zigzag + 1])
            };
            if value <= 0 {
                return Err(jpeg_err("invalid quantization value"));
            }
            quant[table][*slot] = value;
        }
        pos += 64 * bytes_per_value;
    }
    Ok(())
}

fn jpeg_parse_dht(
    data: &[u8],
    dc_tables: &mut [Option<JpegHuffman>; 4],
    ac_tables: &mut [Option<JpegHuffman>; 4],
) -> Result<(), DecodeError> {
    let mut pos = 0;
    while pos < data.len() {
        let spec = data[pos];
        pos += 1;
        let class = spec >> 4;
        let table = usize::from(spec & 0x0f);
        if table >= 4 {
            return Err(jpeg_err("invalid Huffman table selector"));
        }
        let length_bytes = data
            .get(pos..pos + 16)
            .ok_or_else(|| jpeg_err("truncated DHT segment"))?;
        let mut counts = [0_u16; 17];
        let mut total = 0_usize;
        for (index, &count) in length_bytes.iter().enumerate() {
            counts[index + 1] = u16::from(count);
            total += usize::from(count);
        }
        pos += 16;
        let symbol_bytes = data
            .get(pos..pos + total)
            .ok_or_else(|| jpeg_err("truncated DHT segment"))?;
        let table_obj = JpegHuffman::new(counts, symbol_bytes.to_vec())?;
        pos += total;
        match class {
            0 => dc_tables[table] = Some(table_obj),
            1 => ac_tables[table] = Some(table_obj),
            _ => return Err(jpeg_err("invalid Huffman table class")),
        }
    }
    Ok(())
}

fn jpeg_parse_sof0(data: &[u8]) -> Result<JpegFrame, DecodeError> {
    let header = data
        .get(..6)
        .ok_or_else(|| jpeg_err("truncated SOF0 segment"))?;
    if header[0] != 8 {
        return Err(jpeg_unsupported(format!(
            "{}-bit JPEG samples (only 8-bit is supported)",
            header[0]
        )));
    }
    let height = u32::from(u16::from_be_bytes([header[1], header[2]]));
    let width = u32::from(u16::from_be_bytes([header[3], header[4]]));
    if width == 0 || height == 0 {
        return Err(jpeg_err("JPEG has a zero dimension"));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(jpeg_err(format!(
            "dimensions {width}x{height} exceed the {MAX_DIMENSION} pixel limit"
        )));
    }
    check_pixel_count(width, height, "JPEG")?;

    let count = usize::from(header[5]);
    if data.len() < 6 + 3 * count {
        return Err(jpeg_err("truncated SOF0 component list"));
    }
    if count != 1 && count != 3 {
        return Err(jpeg_unsupported(format!("{count}-component JPEG frames")));
    }
    let mut components = Vec::with_capacity(count);
    let mut seen = [false; 256];
    for index in 0..count {
        let id = data[6 + 3 * index];
        let sampling = data[7 + 3 * index];
        let (h, v) = (sampling >> 4, sampling & 0x0f);
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
            return Err(jpeg_unsupported("invalid JPEG sampling factors"));
        }
        if seen[usize::from(id)] {
            return Err(jpeg_err("duplicate JPEG component identifier"));
        }
        seen[usize::from(id)] = true;
        let quant = usize::from(data[8 + 3 * index]);
        if quant >= 4 {
            return Err(jpeg_err("invalid component quantization table selector"));
        }
        components.push(JpegComponent {
            h,
            v,
            quant,
            dc_table: 0,
            ac_table: 0,
        });
    }
    Ok(JpegFrame {
        width,
        height,
        components,
        quant: [[0; 64]; 4],
        restart_interval: 0,
    })
}

/// Decodes the baseline scan into a flat component plane per frame component.
#[allow(clippy::needless_range_loop)]
fn jpeg_decode_scan(
    frame: &JpegFrame,
    entropy: &[u8],
    dc_tables: &[Option<JpegHuffman>; 4],
    ac_tables: &[Option<JpegHuffman>; 4],
    cos: &[[f32; 8]; 8],
) -> Result<ImageData, DecodeError> {
    let max_h = frame
        .components
        .iter()
        .map(|component| component.h)
        .max()
        .unwrap_or(1);
    let max_v = frame
        .components
        .iter()
        .map(|component| component.v)
        .max()
        .unwrap_or(1);
    let mcu_width = usize::from(max_h) * 8;
    let mcu_height = usize::from(max_v) * 8;
    let mcus_x = (frame.width as usize).div_ceil(mcu_width);
    let mcus_y = (frame.height as usize).div_ceil(mcu_height);

    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(frame.components.len());
    for component in &frame.components {
        let plane_width = mcus_x * usize::from(component.h) * 8;
        let plane_height = mcus_y * usize::from(component.v) * 8;
        let size = plane_width
            .checked_mul(plane_height)
            .ok_or_else(|| jpeg_err("JPEG component plane size overflow"))?;
        planes.push(vec![0_u8; size]);
    }

    let mut dc_predictors = vec![0_i32; frame.components.len()];
    let mut reader = JpegBitReader::new(entropy);
    let mut restart_index = 0_u8;
    let mut mcu_index = 0_usize;

    for mcu_y in 0..mcus_y {
        for mcu_x in 0..mcus_x {
            if frame.restart_interval > 0
                && mcu_index > 0
                && mcu_index.is_multiple_of(usize::from(frame.restart_interval))
            {
                reader.restart(0xd0 + restart_index)?;
                restart_index = (restart_index + 1) % 8;
                dc_predictors.fill(0);
            }

            for (component_index, component) in frame.components.iter().enumerate() {
                let dc_table = dc_tables[component.dc_table]
                    .as_ref()
                    .ok_or_else(|| jpeg_err("missing DC Huffman table"))?;
                let ac_table = ac_tables[component.ac_table]
                    .as_ref()
                    .ok_or_else(|| jpeg_err("missing AC Huffman table"))?;
                for block_y in 0..component.v {
                    for block_x in 0..component.h {
                        let mut coefficients = [0_i32; 64];
                        let category = usize::from(dc_table.decode(&mut reader)?);
                        dc_predictors[component_index] +=
                            jpeg_receive_extend(category, &mut reader)?;
                        coefficients[0] = dc_predictors[component_index];

                        let mut zigzag = 1_usize;
                        while zigzag < 64 {
                            let symbol = ac_table.decode(&mut reader)?;
                            if symbol == 0x00 {
                                break; // end of block
                            }
                            if symbol == 0xf0 {
                                zigzag += 16; // zero run length
                                continue;
                            }
                            let run = usize::from(symbol >> 4);
                            let size = usize::from(symbol & 0x0f);
                            if size == 0 || zigzag + run >= 64 {
                                return Err(jpeg_err("invalid AC coefficient run"));
                            }
                            let value = jpeg_receive_extend(size, &mut reader)?;
                            zigzag += run;
                            coefficients[JPEG_ZIGZAG[zigzag]] = value;
                            zigzag += 1;
                        }

                        let quant = &frame.quant[component.quant];
                        for (coefficient, q) in coefficients.iter_mut().zip(quant.iter()) {
                            *coefficient *= *q;
                        }
                        let block = jpeg_idct(&coefficients, cos);

                        let plane = &mut planes[component_index];
                        let plane_width = mcus_x * usize::from(component.h) * 8;
                        let origin_x =
                            (mcu_x * usize::from(component.h) + usize::from(block_x)) * 8;
                        let origin_y =
                            (mcu_y * usize::from(component.v) + usize::from(block_y)) * 8;
                        for y in 0..8 {
                            for x in 0..8 {
                                plane[(origin_y + y) * plane_width + origin_x + x] = block[y][x];
                            }
                        }
                    }
                }
            }
            mcu_index += 1;
        }
    }

    let width = frame.width as usize;
    let height = frame.height as usize;
    let mut pixels = vec![0_u8; width * height * 4];
    #[allow(clippy::needless_range_loop)]
    for y in 0..height {
        for x in 0..width {
            let sample = |index: usize| -> i32 {
                let component = &frame.components[index];
                let sx = x * usize::from(component.h) / usize::from(max_h);
                let sy = y * usize::from(component.v) / usize::from(max_v);
                let plane = &planes[index];
                let plane_width = mcus_x * usize::from(component.h) * 8;
                i32::from(plane[sy * plane_width + sx])
            };
            let target = (y * width + x) * 4;
            if frame.components.len() == 1 {
                let grey = sample(0).clamp(0, 255) as u8;
                pixels[target..target + 4].copy_from_slice(&[grey, grey, grey, 255]);
            } else {
                let luma = sample(0) as f32;
                let blue = sample(1) as f32 - 128.0;
                let red = sample(2) as f32 - 128.0;
                let r = (luma + 1.402 * red).round().clamp(0.0, 255.0) as u8;
                let g = (luma - 0.344_136 * blue - 0.714_136 * red)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let b = (luma + 1.772 * blue).round().clamp(0.0, 255.0) as u8;
                pixels[target..target + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
    ImageData::from_rgba(frame.width, frame.height, pixels)
}

/// Separable inverse DCT for one 8x8 block. `cos[x][u]` is
/// `alpha(u) * cos((2x + 1) * u * pi / 16)` with `alpha(0) = 1/sqrt(2)`.
#[allow(clippy::needless_range_loop)]
fn jpeg_idct(coefficients: &[i32; 64], cos: &[[f32; 8]; 8]) -> [[u8; 8]; 8] {
    let mut intermediate = [[0_f32; 8]; 8];
    for row in 0..8 {
        for x in 0..8 {
            let mut sum = 0_f32;
            for column in 0..8 {
                sum += coefficients[row * 8 + column] as f32 * cos[x][column];
            }
            intermediate[row][x] = sum;
        }
    }
    let mut block = [[0_u8; 8]; 8];
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0_f32;
            for row in 0..8 {
                sum += intermediate[row][x] * cos[y][row];
            }
            let value = (sum * 0.25 + 128.0).round().clamp(0.0, 255.0) as u8;
            block[y][x] = value;
        }
    }
    block
}

/// Precomputes the cosine factors used by [`jpeg_idct`].
fn jpeg_idct_cosines() -> [[f32; 8]; 8] {
    let mut cos = [[0_f32; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, slot) in row.iter_mut().enumerate() {
            let alpha = if u == 0 { 1.0 / 2.0_f32.sqrt() } else { 1.0 };
            let angle = ((2 * x + 1) as f32) * (u as f32) * std::f32::consts::PI / 16.0;
            *slot = alpha * angle.cos();
        }
    }
    cos
}

/// Decodes a baseline sequential DCT (SOF0) JPEG into RGBA8.
///
/// Greyscale and three-component YCbCr images are supported with any legal
/// sampling factors (including 4:4:4, 4:2:2 and 4:2:0), Huffman entropy coding,
/// restart intervals and 8- or 16-bit quantization tables. Progressive (SOF2),
/// extended-sequential, lossless, arithmetic-coded, 12-bit and multi-scan
/// JPEGs are reported as unsupported via [`DecodeError::unsupported`].
pub fn decode_jpeg(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err(jpeg_err("not a JPEG file (missing SOI)"));
    }
    let mut pos = 2_usize;
    let mut quant = [[0_i32; 64]; 4];
    let mut dc_tables: [Option<JpegHuffman>; 4] = Default::default();
    let mut ac_tables: [Option<JpegHuffman>; 4] = Default::default();
    let mut frame: Option<JpegFrame> = None;
    let mut restart_interval = 0_u16;

    loop {
        let marker = jpeg_next_marker(bytes, &mut pos)?;
        match marker {
            0xd8 => return Err(jpeg_err("unexpected SOI marker")),
            0xd9 => return Err(jpeg_err("missing SOS marker")),
            0x01 | 0xd0..=0xd7 => return Err(jpeg_err("unexpected restart marker in header")),
            0xe0..=0xef | 0xfe => {
                let _ = jpeg_segment(bytes, &mut pos, "APP/COM")?;
            }
            0xdb => {
                let segment = jpeg_segment(bytes, &mut pos, "DQT")?;
                jpeg_parse_dqt(segment, &mut quant)?;
            }
            0xc4 => {
                let segment = jpeg_segment(bytes, &mut pos, "DHT")?;
                jpeg_parse_dht(segment, &mut dc_tables, &mut ac_tables)?;
            }
            0xdd => {
                let segment = jpeg_segment(bytes, &mut pos, "DRI")?;
                let bytes = segment
                    .get(..2)
                    .ok_or_else(|| jpeg_err("truncated DRI segment"))?;
                restart_interval = u16::from_be_bytes([bytes[0], bytes[1]]);
            }
            0xc0 => {
                let segment = jpeg_segment(bytes, &mut pos, "SOF0")?;
                frame = Some(jpeg_parse_sof0(segment)?);
            }
            0xc1 => return Err(jpeg_unsupported("extended sequential DCT (SOF1)")),
            0xc2 => return Err(jpeg_unsupported("progressive DCT (SOF2)")),
            0xc3 => return Err(jpeg_unsupported("lossless JPEG (SOF3)")),
            0xc5..=0xcf => {
                return Err(jpeg_unsupported(format!(
                    "unsupported frame type 0x{marker:02x}"
                )));
            }
            0xda => {
                let segment = jpeg_segment(bytes, &mut pos, "SOS")?;
                let count = segment
                    .first()
                    .copied()
                    .ok_or_else(|| jpeg_err("empty SOS"))?;
                let mut frame = frame.take().ok_or_else(|| jpeg_err("SOS before SOF"))?;
                if usize::from(count) != frame.components.len() {
                    return Err(jpeg_unsupported("non-interleaved or multi-scan JPEG"));
                }
                let header_end = 1 + 2 * usize::from(count);
                let scan_trailer = segment
                    .get(header_end..header_end + 3)
                    .ok_or_else(|| jpeg_err("truncated SOS header"))?;
                if scan_trailer != [0, 63, 0] {
                    return Err(jpeg_unsupported(
                        "progressive spectral selection or successive approximation",
                    ));
                }
                for (index, component) in frame.components.iter_mut().enumerate() {
                    let spec = segment
                        .get(2 + 2 * index)
                        .ok_or_else(|| jpeg_err("truncated SOS component list"))?;
                    component.dc_table = usize::from(spec >> 4);
                    component.ac_table = usize::from(spec & 0x0f);
                    if component.dc_table >= 4 || component.ac_table >= 4 {
                        return Err(jpeg_err("invalid scan Huffman table selector"));
                    }
                }
                frame.quant = quant;
                frame.restart_interval = restart_interval;
                let cos = jpeg_idct_cosines();
                return jpeg_decode_scan(&frame, &bytes[pos..], &dc_tables, &ac_tables, &cos);
            }
            other => {
                return Err(jpeg_unsupported(format!("marker 0x{other:02x}")));
            }
        }
    }
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
        interlace: u8,
        extra: &[(&[u8; 4], &[u8])],
        idat: &[u8],
    ) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[bit_depth, color_type, 0, 0, interlace]);
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
            0,
            extra,
            &zlib_stored(raw),
        )
    }

    fn png_with_raw_interlaced(
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
            1,
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
            let file = assemble_png(30, 20, 8, 6, 0, &[], &zlib);
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

        // An invalid interlace method is rejected by the header parser.
        let mut header = png_with_raw(1, 1, 8, 6, &[], &[0, 1, 2, 3, 4]);
        let ihdr_data = 8 + 8;
        header[ihdr_data + 12] = 2;
        let crc_start = 8 + 8 + 13;
        let mut crc_input = Vec::new();
        crc_input.extend_from_slice(b"IHDR");
        crc_input.extend_from_slice(&header[ihdr_data..ihdr_data + 13]);
        let crc = crc32(&crc_input).to_be_bytes();
        header[crc_start..crc_start + 4].copy_from_slice(&crc);
        assert!(decode_png(&header).is_err());
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
        let gif = decode(b"GIF89a\x01\x00\x01\x00").expect_err("truncated GIF is rejected");
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

    // ---------------------------------------------------------------------
    // GIF test helpers
    // ---------------------------------------------------------------------

    struct GifFrameSpec {
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        indices: Vec<u8>,
        interlace: bool,
        palette: Option<Vec<[u8; 3]>>,
        min_code_size: u8,
    }

    /// Low three bits of a GIF packed field for a table of `entries` colours.
    fn gif_table_bits(entries: usize) -> u8 {
        let mut size = entries.next_power_of_two().max(2);
        let mut bits = 0_u8;
        while size > 2 {
            size >>= 1;
            bits += 1;
        }
        bits
    }

    struct GifBitWriter {
        out: Vec<u8>,
        accumulator: u32,
        bits: u32,
    }

    impl GifBitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                accumulator: 0,
                bits: 0,
            }
        }

        fn write(&mut self, value: u32, width: u32) {
            let mask = if width >= 32 {
                u32::MAX
            } else {
                (1_u32 << width) - 1
            };
            self.accumulator |= (value & mask) << self.bits;
            self.bits += width;
            while self.bits >= 8 {
                self.out.push((self.accumulator & 0xff) as u8);
                self.accumulator >>= 8;
                self.bits -= 8;
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.bits > 0 {
                self.out.push((self.accumulator & 0xff) as u8);
            }
            self.out
        }
    }

    /// Minimal GIF LZW encoder. It emits each index as a literal and mirrors the
    /// decoder's dictionary growth so code widths stay in sync without needing
    /// a real match-finding dictionary.
    fn gif_lzw_encode(indices: &[u8], min_code_size: u8) -> Vec<u8> {
        let clear = 1_u32 << min_code_size;
        let end_of_information = clear + 1;
        let mut writer = GifBitWriter::new();
        let mut code_size = u32::from(min_code_size) + 1;
        let mut next_code = end_of_information + 1;
        let mut have_previous = false;
        writer.write(clear, code_size);
        for &index in indices {
            writer.write(u32::from(index), code_size);
            if have_previous && next_code < 4096 {
                next_code += 1;
                if next_code == (1_u32 << code_size) && code_size < 12 {
                    code_size += 1;
                }
            }
            have_previous = true;
        }
        writer.write(end_of_information, code_size);
        writer.finish()
    }

    fn build_gif(
        version: &[u8; 6],
        width: u16,
        height: u16,
        global_table: Option<&[[u8; 3]]>,
        background: u8,
        transparent: Option<u8>,
        frames: &[GifFrameSpec],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(version);
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        let mut packed = 0_u8;
        if let Some(table) = global_table {
            packed |= 0x80 | 0x70 | gif_table_bits(table.len());
        }
        out.push(packed);
        out.push(background);
        out.push(0);
        if let Some(table) = global_table {
            for entry in table {
                out.extend_from_slice(entry);
            }
        }
        if let Some(index) = transparent {
            // Graphic Control Extension: flags, delay, transparent index.
            out.extend_from_slice(&[0x21, 0xf9, 0x04, 0x01, 0, 0, index, 0]);
        }
        for frame in frames {
            out.push(0x2c);
            out.extend_from_slice(&frame.left.to_le_bytes());
            out.extend_from_slice(&frame.top.to_le_bytes());
            out.extend_from_slice(&frame.width.to_le_bytes());
            out.extend_from_slice(&frame.height.to_le_bytes());
            let mut frame_packed = 0_u8;
            if let Some(table) = &frame.palette {
                frame_packed |= 0x80 | gif_table_bits(table.len());
            }
            if frame.interlace {
                frame_packed |= 0x40;
            }
            out.push(frame_packed);
            if let Some(table) = &frame.palette {
                for entry in table {
                    out.extend_from_slice(entry);
                }
            }
            out.push(frame.min_code_size);
            let data = gif_lzw_encode(&frame.indices, frame.min_code_size);
            for chunk in data.chunks(255) {
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
            out.push(0);
        }
        out.push(0x3b);
        out
    }

    fn solid_frame(width: u16, height: u16, index: u8) -> GifFrameSpec {
        GifFrameSpec {
            left: 0,
            top: 0,
            width,
            height,
            indices: vec![index; usize::from(width) * usize::from(height)],
            interlace: false,
            palette: None,
            min_code_size: 2,
        }
    }

    const RED: [u8; 3] = [255, 0, 0];
    const GREEN: [u8; 3] = [0, 255, 0];
    const BLUE: [u8; 3] = [0, 0, 255];
    const WHITE: [u8; 3] = [255, 255, 255];

    #[test]
    fn decodes_single_colour_gif() {
        let table = [RED, GREEN];
        let frames = [solid_frame(1, 1, 0)];
        let file = build_gif(GIF_SIGNATURE_89A, 1, 1, Some(&table), 0, None, &frames);
        let image = decode(&file).expect("solid GIF should decode");
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.get(0, 0), [255, 0, 0, 255]);
        assert_eq!(ImageData::decode(&file).expect("convenience decode"), image);
    }

    #[test]
    fn decodes_two_colour_gif() {
        let table = [RED, GREEN];
        let mut frame = solid_frame(3, 1, 0);
        frame.indices = vec![0, 1, 0];
        let file = build_gif(GIF_SIGNATURE_89A, 3, 1, Some(&table), 0, None, &[frame]);
        let image = decode(&file).expect("two-colour GIF should decode");
        assert_eq!(image.get(0, 0), [255, 0, 0, 255]);
        assert_eq!(image.get(1, 0), [0, 255, 0, 255]);
        assert_eq!(image.get(2, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn decodes_gif_transparency() {
        let table = [RED, GREEN];
        let mut frame = solid_frame(2, 1, 0);
        frame.indices = vec![0, 1];
        let file = build_gif(GIF_SIGNATURE_89A, 2, 1, Some(&table), 0, Some(1), &[frame]);
        let image = decode(&file).expect("transparent GIF should decode");
        assert_eq!(image.get(0, 0), [255, 0, 0, 255]);
        assert_eq!(image.get(1, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn decodes_interlaced_gif() {
        let table: Vec<[u8; 3]> = (0..8).map(|value| [value * 32; 3]).collect();
        let width = 8_usize;
        let height = 8_usize;
        let rows: Vec<Vec<u8>> = (0..height)
            .map(|_| (0..width).map(|_| 0).collect())
            .collect();
        let mut image_rows = rows;
        for (y, row) in image_rows.iter_mut().enumerate() {
            for value in row.iter_mut() {
                *value = y as u8;
            }
        }
        let order = gif_interlaced_rows(height);
        let indices: Vec<u8> = order
            .iter()
            .flat_map(|&row| image_rows[row].clone())
            .collect();
        let frame = GifFrameSpec {
            left: 0,
            top: 0,
            width: width as u16,
            height: height as u16,
            indices,
            interlace: true,
            palette: None,
            min_code_size: 3,
        };
        let file = build_gif(
            GIF_SIGNATURE_89A,
            width as u16,
            height as u16,
            Some(&table),
            0,
            None,
            &[frame],
        );
        let image = decode(&file).expect("interlaced GIF should decode");
        for y in 0..height {
            for x in 0..width {
                let value = (y as u8) * 32;
                assert_eq!(
                    image.get(x as u32, y as u32),
                    [value, value, value, 255],
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn decodes_gif_with_lzw_code_size_growth() {
        let table: Vec<[u8; 3]> = (0..256)
            .map(|value| [value as u8, (255 - value) as u8, (value / 2) as u8])
            .collect();
        let width = 40_usize;
        let height = 40_usize;
        let mut indices = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                indices.push(((x + y * width) % 256) as u8);
            }
        }
        let frame = GifFrameSpec {
            left: 0,
            top: 0,
            width: width as u16,
            height: height as u16,
            indices,
            interlace: false,
            palette: None,
            min_code_size: 8,
        };
        let file = build_gif(
            GIF_SIGNATURE_89A,
            width as u16,
            height as u16,
            Some(&table),
            0,
            None,
            &[frame],
        );
        let image = decode(&file).expect("GIF with growing code size should decode");
        for y in 0..height {
            for x in 0..width {
                let index = ((x + y * width) % 256) as u8;
                let expected = [index, 255_u8.wrapping_sub(index), index / 2, 255];
                assert_eq!(image.get(x as u32, y as u32), expected, "at {x},{y}");
            }
        }
    }

    #[test]
    fn decodes_gif_versions_87a_and_89a() {
        let table = [RED, GREEN];
        for version in [GIF_SIGNATURE_87A, GIF_SIGNATURE_89A] {
            let file = build_gif(
                version,
                1,
                1,
                Some(&table),
                0,
                None,
                &[solid_frame(1, 1, 0)],
            );
            let image = decode(&file).expect("GIF should decode");
            assert_eq!(image.get(0, 0), [255, 0, 0, 255]);
        }
    }

    #[test]
    fn gif_skips_unknown_extensions() {
        let table = [RED, GREEN];
        let file = build_gif(
            GIF_SIGNATURE_89A,
            1,
            1,
            Some(&table),
            0,
            None,
            &[solid_frame(1, 1, 0)],
        );
        // Header + logical screen + 2-entry global table.
        let insertion = 6 + 7 + 6;
        let mut spliced = file[..insertion].to_vec();
        // Comment extension: label 0xFE, one sub-block, terminator.
        spliced.extend_from_slice(&[0x21, 0xfe, 3, b'h', b'i', b'!', 0]);
        // Application extension with an 11-byte identifier.
        spliced.extend_from_slice(&[0x21, 0xff, 11]);
        spliced.extend_from_slice(b"NETSCAPE2.0");
        spliced.extend_from_slice(&[3, 1, 0, 0, 0]);
        spliced.extend_from_slice(&file[insertion..]);
        let image = decode(&spliced).expect("extensions should be skipped");
        assert_eq!(image.get(0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn gif_fills_background_and_clips_frames() {
        let table = [RED, GREEN, BLUE, WHITE];
        let frame = GifFrameSpec {
            left: 1,
            top: 1,
            width: 2,
            height: 2,
            indices: vec![0; 4],
            interlace: false,
            palette: None,
            min_code_size: 2,
        };
        let file = build_gif(
            GIF_SIGNATURE_89A,
            4,
            4,
            Some(&table),
            2, // blue background
            None,
            &[frame],
        );
        let image = decode(&file).expect("GIF with offset frame should decode");
        assert_eq!(image.get(0, 0), [0, 0, 255, 255]);
        assert_eq!(image.get(3, 3), [0, 0, 255, 255]);
        assert_eq!(image.get(1, 1), [255, 0, 0, 255]);
        assert_eq!(image.get(2, 2), [255, 0, 0, 255]);
    }

    #[test]
    fn gif_local_colour_table_overrides_global() {
        let global = [RED, GREEN];
        let local = vec![[0_u8, 255, 0], [255, 255, 255]];
        let frame = GifFrameSpec {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            indices: vec![0],
            interlace: false,
            palette: Some(local),
            min_code_size: 2,
        };
        let file = build_gif(GIF_SIGNATURE_89A, 1, 1, Some(&global), 0, None, &[frame]);
        let image = decode(&file).expect("local colour table should decode");
        assert_eq!(image.get(0, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn gif_returns_first_frame_only() {
        let table = [RED, GREEN];
        let first = solid_frame(1, 1, 0);
        let second = solid_frame(1, 1, 1);
        let file = build_gif(
            GIF_SIGNATURE_89A,
            1,
            1,
            Some(&table),
            0,
            None,
            &[first, second],
        );
        let image = decode(&file).expect("multi-frame GIF should decode its first frame");
        assert_eq!(image.get(0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn rejects_malformed_gifs() {
        let table = [RED, GREEN];
        let valid = build_gif(
            GIF_SIGNATURE_89A,
            4,
            4,
            Some(&table),
            0,
            None,
            &[solid_frame(4, 4, 0)],
        );

        // Truncated inside the frame data.
        let mut truncated = valid.clone();
        truncated.truncate(valid.len() * 3 / 5);
        assert!(decode(&truncated).is_err());

        // Bad signature: the file is no longer detected as a GIF.
        let mut bad_signature = valid.clone();
        bad_signature[4] = b'X';
        let error = decode(&bad_signature).expect_err("bad GIF signature must be rejected");
        assert!(error.message.contains("unrecognised"));

        // Oversized logical screen.
        let oversized = build_gif(
            GIF_SIGNATURE_89A,
            (MAX_DIMENSION + 1) as u16,
            1,
            Some(&table),
            0,
            None,
            &[],
        );
        assert!(decode(&oversized).is_err());

        // Zero-sized logical screen.
        let zero = build_gif(GIF_SIGNATURE_89A, 0, 1, Some(&table), 0, None, &[]);
        assert!(decode(&zero).is_err());

        // No frame in the file.
        let empty = build_gif(GIF_SIGNATURE_89A, 1, 1, Some(&table), 0, None, &[]);
        assert!(decode(&empty).is_err());

        // Frame without any colour table.
        let no_table = build_gif(
            GIF_SIGNATURE_89A,
            1,
            1,
            None,
            0,
            None,
            &[solid_frame(1, 1, 0)],
        );
        assert!(decode(&no_table).is_err());

        // Frame with a zero dimension.
        let zero_frame = build_gif(
            GIF_SIGNATURE_89A,
            1,
            1,
            Some(&table),
            0,
            None,
            &[GifFrameSpec {
                left: 0,
                top: 0,
                width: 0,
                height: 1,
                indices: vec![0],
                interlace: false,
                palette: None,
                min_code_size: 2,
            }],
        );
        assert!(decode(&zero_frame).is_err());
    }

    #[test]
    fn gif_lzw_rejects_invalid_streams() {
        assert!(gif_lzw_decode(&[], 2, 4).is_err());
        assert!(gif_lzw_decode(&[0; 4], 9, 4).is_err());
        assert!(gif_lzw_decode(&[0xff; 8], 2, 100).is_err());
        // A clear code followed immediately by end-of-information has no pixels.
        // min code size 2 => 3-bit codes: clear = 4 (0b100), EOI = 5 (0b101).
        let clear_then_eoi = [0b0010_1100_u8];
        assert!(gif_lzw_decode(&clear_then_eoi, 2, 4).is_err());
    }

    // ---------------------------------------------------------------------
    // PNG bit-depth and Adam7 test helpers
    // ---------------------------------------------------------------------

    fn pack_samples(samples: &[u8], bit_depth: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = 0_u8;
        let mut filled = 0_u32;
        for &sample in samples {
            byte = (byte << bit_depth) | (sample & ((1_u8 << bit_depth) - 1));
            filled += u32::from(bit_depth);
            if filled == 8 {
                out.push(byte);
                byte = 0;
                filled = 0;
            }
        }
        if filled > 0 {
            out.push(byte << (8 - filled));
        }
        out
    }

    /// Builds unfiltered Adam7 scanlines from a per-pixel sample callback.
    fn adam7_raw(
        width: usize,
        height: usize,
        bit_depth: u8,
        channels: usize,
        pixel: &dyn Fn(usize, usize) -> Vec<u8>,
        filter: &dyn Fn(usize, usize) -> u8,
    ) -> Vec<u8> {
        let bpp = png_filter_bpp(channels, bit_depth);
        let mut raw = Vec::new();
        for (pass_index, pass) in ADAM7_PASSES.iter().enumerate() {
            let pass_width = pass_extent(width, pass.x_start, pass.x_step);
            let pass_height = pass_extent(height, pass.y_start, pass.y_step);
            if pass_width == 0 || pass_height == 0 {
                continue;
            }
            let row_bytes = png_row_bytes(pass_width, channels, bit_depth).expect("pass row size");
            let mut previous = vec![0_u8; row_bytes];
            for row in 0..pass_height {
                let mut samples = Vec::new();
                for column in 0..pass_width {
                    let x = pass.x_start + column * pass.x_step;
                    let y = pass.y_start + row * pass.y_step;
                    samples.extend_from_slice(&pixel(x, y));
                }
                let row_buffer = if bit_depth < 8 {
                    pack_samples(&samples, bit_depth)
                } else {
                    samples
                };
                raw.extend_from_slice(&filter_row(
                    filter(pass_index, row),
                    &row_buffer,
                    &previous,
                    bpp,
                ));
                previous = row_buffer;
            }
        }
        raw
    }

    #[test]
    fn decodes_interlaced_rgba_png() {
        let width = 9_usize;
        let height = 9_usize;
        let pixel = |x: usize, y: usize| {
            vec![
                (x * 23) as u8,
                (y * 31) as u8,
                ((x * 7 + y * 3) % 256) as u8,
                if (x + y).is_multiple_of(3) { 128 } else { 255 },
            ]
        };
        let raw = adam7_raw(width, height, 8, 4, &pixel, &|_, _| 0);
        let file = png_with_raw_interlaced(width as u32, height as u32, 8, 6, &[], &raw);
        let image = decode_png(&file).expect("interlaced RGBA PNG should decode");
        for y in 0..height {
            for x in 0..width {
                let expected = pixel(x, y);
                assert_eq!(
                    image.get(x as u32, y as u32),
                    [expected[0], expected[1], expected[2], expected[3]],
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn decodes_interlaced_rgb_png() {
        let width = 10_usize;
        let height = 7_usize;
        let pixel = |x: usize, y: usize| vec![(x * 11) as u8, (y * 17) as u8, (x + y) as u8];
        let raw = adam7_raw(width, height, 8, 3, &pixel, &|_, _| 0);
        let file = png_with_raw_interlaced(width as u32, height as u32, 8, 2, &[], &raw);
        let image = decode_png(&file).expect("interlaced RGB PNG should decode");
        for y in 0..height {
            for x in 0..width {
                let expected = pixel(x, y);
                assert_eq!(
                    image.get(x as u32, y as u32),
                    [expected[0], expected[1], expected[2], 255],
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn decodes_interlaced_grey_png() {
        let width = 8_usize;
        let height = 8_usize;
        let pixel = |x: usize, y: usize| vec![((x * 19 + y * 29) % 256) as u8];
        let raw = adam7_raw(width, height, 8, 1, &pixel, &|_, _| 0);
        let file = png_with_raw_interlaced(width as u32, height as u32, 8, 0, &[], &raw);
        let image = decode_png(&file).expect("interlaced grey PNG should decode");
        for y in 0..height {
            for x in 0..width {
                let value = pixel(x, y)[0];
                assert_eq!(image.get(x as u32, y as u32), [value, value, value, 255]);
            }
        }
    }

    #[test]
    fn decodes_interlaced_palette_png() {
        let palette: Vec<[u8; 3]> = vec![RED, GREEN, BLUE, WHITE];
        let mut plte = Vec::new();
        for entry in &palette {
            plte.extend_from_slice(entry);
        }
        let width = 8_usize;
        let height = 8_usize;
        let pixel = |x: usize, y: usize| vec![((x + y) % 4) as u8];
        let raw = adam7_raw(width, height, 8, 1, &pixel, &|_, _| 0);
        let file = png_with_raw_interlaced(
            width as u32,
            height as u32,
            8,
            3,
            &[(b"PLTE", &plte), (b"tRNS", &[0, 85, 170, 255])],
            &raw,
        );
        let image = decode_png(&file).expect("interlaced palette PNG should decode");
        let alphas = [0_u8, 85, 170, 255];
        for y in 0..height {
            for x in 0..width {
                let index = (x + y) % 4;
                let entry = palette[index];
                assert_eq!(
                    image.get(x as u32, y as u32),
                    [entry[0], entry[1], entry[2], alphas[index]]
                );
            }
        }
    }

    #[test]
    fn decodes_interlaced_png_with_per_pass_filters() {
        let width = 16_usize;
        let height = 16_usize;
        let pixel = |x: usize, y: usize| {
            vec![
                (x * 13) as u8,
                (y * 11) as u8,
                ((x * y) % 256) as u8,
                (255 - ((x + y) % 64)) as u8,
            ]
        };
        // Every pass uses its own sequence of filter types; filters must never
        // see scanlines from another pass.
        let raw = adam7_raw(width, height, 8, 4, &pixel, &|pass, row| {
            ((pass + row) % 5) as u8
        });
        let file = png_with_raw_interlaced(width as u32, height as u32, 8, 6, &[], &raw);
        let image = decode_png(&file).expect("filtered interlaced PNG should decode");
        for y in 0..height {
            for x in 0..width {
                let expected = pixel(x, y);
                assert_eq!(
                    image.get(x as u32, y as u32),
                    [expected[0], expected[1], expected[2], expected[3]],
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn decodes_1_bit_grey_png() {
        let samples = [0_u8, 1, 0, 1, 1, 0, 1, 0];
        let row = pack_samples(&samples, 1);
        let raw = [0_u8, row[0], 0, row[0]];
        let file = png_with_raw(8, 2, 1, 0, &[], &raw);
        let image = decode_png(&file).expect("1-bit grey PNG should decode");
        for (x, &sample) in samples.iter().enumerate() {
            let expected = if sample == 0 { 0 } else { 255 };
            assert_eq!(image.get(x as u32, 0), [expected, expected, expected, 255]);
            assert_eq!(image.get(x as u32, 1), [expected, expected, expected, 255]);
        }
    }

    #[test]
    fn decodes_2_bit_grey_png() {
        let samples = [0_u8, 1, 2, 3];
        let raw = [0_u8, pack_samples(&samples, 2)[0]];
        let file = png_with_raw(4, 1, 2, 0, &[], &raw);
        let image = decode_png(&file).expect("2-bit grey PNG should decode");
        for (x, &sample) in samples.iter().enumerate() {
            let value = sample * 85;
            assert_eq!(image.get(x as u32, 0), [value, value, value, 255]);
        }
    }

    #[test]
    fn decodes_4_bit_grey_png() {
        let samples = [0_u8, 1, 7, 15];
        let packed = pack_samples(&samples, 4);
        let raw = [0_u8, packed[0], packed[1]];
        let file = png_with_raw(4, 1, 4, 0, &[], &raw);
        let image = decode_png(&file).expect("4-bit grey PNG should decode");
        for (x, &sample) in samples.iter().enumerate() {
            let value = sample * 17;
            assert_eq!(image.get(x as u32, 0), [value, value, value, 255]);
        }
    }

    #[test]
    fn decodes_sub_byte_grey_with_filters() {
        let width = 4_usize;
        let rows: [[u8; 4]; 4] = [[0, 1, 2, 3], [4, 5, 6, 7], [8, 9, 10, 11], [12, 13, 14, 15]];
        let mut raw = Vec::new();
        let mut previous = vec![0_u8; 2];
        for (row_index, row) in rows.iter().enumerate() {
            let packed = pack_samples(row, 4);
            raw.extend_from_slice(&filter_row(row_index as u8 + 1, &packed, &previous, 1));
            previous = packed;
        }
        let file = png_with_raw(width as u32, 4, 4, 0, &[], &raw);
        let image = decode_png(&file).expect("filtered 4-bit grey PNG should decode");
        for (y, row) in rows.iter().enumerate() {
            for (x, &sample) in row.iter().enumerate() {
                let value = sample * 17;
                assert_eq!(
                    image.get(x as u32, y as u32),
                    [value, value, value, 255],
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn decodes_1_bit_palette_with_transparency() {
        let plte = [255_u8, 0, 0, 0, 255, 0];
        let samples = [0_u8, 1, 0, 1, 0, 1, 0, 1];
        let raw = [0_u8, pack_samples(&samples, 1)[0]];
        let file = png_with_raw(8, 1, 1, 3, &[(b"PLTE", &plte), (b"tRNS", &[0, 255])], &raw);
        let image = decode_png(&file).expect("1-bit palette PNG should decode");
        assert_eq!(image.get(0, 0), [255, 0, 0, 0]);
        assert_eq!(image.get(1, 0), [0, 255, 0, 255]);
        assert_eq!(image.get(2, 0), [255, 0, 0, 0]);
        assert_eq!(image.get(7, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn decodes_2_bit_palette_png() {
        let plte = [RED, GREEN, BLUE, WHITE].concat();
        let samples = [0_u8, 1, 2, 3];
        let raw = [0_u8, pack_samples(&samples, 2)[0]];
        let file = png_with_raw(4, 1, 2, 3, &[(b"PLTE", &plte)], &raw);
        let image = decode_png(&file).expect("2-bit palette PNG should decode");
        let table = [RED, GREEN, BLUE, WHITE];
        for (x, entry) in table.iter().enumerate() {
            assert_eq!(image.get(x as u32, 0), [entry[0], entry[1], entry[2], 255]);
        }
    }

    #[test]
    fn decodes_4_bit_palette_with_transparency() {
        let plte = [RED, GREEN, BLUE, WHITE].concat();
        let samples = [0_u8, 1, 2, 3];
        let packed = pack_samples(&samples, 4);
        let raw = [0_u8, packed[0], packed[1]];
        let file = png_with_raw(
            4,
            1,
            4,
            3,
            &[(b"PLTE", &plte), (b"tRNS", &[10, 20, 30, 40])],
            &raw,
        );
        let image = decode_png(&file).expect("4-bit palette PNG should decode");
        let alphas = [10_u8, 20, 30, 40];
        let table = [RED, GREEN, BLUE, WHITE];
        for (x, entry) in table.iter().enumerate() {
            assert_eq!(
                image.get(x as u32, 0),
                [entry[0], entry[1], entry[2], alphas[x]]
            );
        }
    }

    #[test]
    fn rejects_bad_sub_byte_pngs() {
        // Sub-byte palette index outside the table.
        let plte = [RED, GREEN].concat();
        let samples = [0_u8, 1, 3, 0];
        let raw = [0_u8, pack_samples(&samples, 2)[0]];
        let file = png_with_raw(4, 1, 2, 3, &[(b"PLTE", &plte)], &raw);
        assert!(decode_png(&file).is_err());

        // Too few decompressed bytes for a sub-byte row.
        let short = png_with_raw(8, 1, 1, 0, &[], &[0]);
        assert!(decode_png(&short).is_err());

        // 16-bit palette data is not supported.
        let deep = png_with_raw(1, 1, 16, 3, &[(b"PLTE", &[1, 2, 3])], &[0, 0, 0]);
        assert!(decode_png(&deep).is_err());
    }

    #[test]
    fn fuzz_mutated_gif_never_panics() {
        let table = [RED, GREEN, BLUE, WHITE];
        let order = gif_interlaced_rows(4);
        let indices: Vec<u8> = order
            .iter()
            .flat_map(|&row| (0..4).map(move |x| ((row + x) % 4) as u8))
            .collect();
        let frame = GifFrameSpec {
            left: 0,
            top: 0,
            width: 4,
            height: 4,
            indices,
            interlace: true,
            palette: None,
            min_code_size: 2,
        };
        let base = build_gif(GIF_SIGNATURE_89A, 4, 4, Some(&table), 2, Some(3), &[frame]);
        let mut state = 0x0bad_f00d_dead_beef_u64;
        for _ in 0..400 {
            let mut next = || {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) as u8
            };
            let mut file = base.clone();
            for _ in 0..4 {
                let index = (next() as usize) % file.len();
                file[index] ^= next();
            }
            let _ = decode(&file);
            let cut = (next() as usize) % base.len();
            let _ = decode(&base[..cut]);
        }
    }

    #[test]
    fn fuzz_mutated_interlaced_and_sub_byte_pngs_never_panic() {
        let rgba = |x: usize, y: usize| vec![(x * 9) as u8, (y * 5) as u8, 0x40, 0xff];
        let interlaced = png_with_raw_interlaced(
            8,
            8,
            8,
            6,
            &[],
            &adam7_raw(8, 8, 8, 4, &rgba, &|pass, row| ((pass + row) % 5) as u8),
        );
        let grey_samples: Vec<u8> = (0..16).map(|value| (value % 16) as u8).collect();
        let sub_byte = png_with_raw(
            16,
            1,
            4,
            0,
            &[],
            &[
                0,
                pack_samples(&grey_samples, 4)[0],
                pack_samples(&grey_samples, 4)[1],
            ],
        );

        let mut state = 0x1234_abcd_9876_5432_u64;
        for (iteration, base) in [interlaced, sub_byte].iter().enumerate() {
            for _ in 0..300 {
                let mut next = || {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    (state >> 33) as u8
                };
                let mut file = base.clone();
                for _ in 0..5 {
                    let index = (next() as usize) % file.len();
                    file[index] ^= next();
                }
                let _ = decode(&file);
                let _ = decode_png(&file);
                let cut = (next() as usize) % base.len();
                let _ = decode(&base[..cut]);
                if iteration == 0 {
                    // Also repair the CRC occasionally so mutated IHDR/IDAT
                    // bytes reach the deeper decode paths.
                    let mut repaired = file.clone();
                    if repaired.len() > 33 {
                        let ihdr = &repaired[16..29];
                        let crc_start = 29;
                        let mut crc_input = Vec::new();
                        crc_input.extend_from_slice(b"IHDR");
                        crc_input.extend_from_slice(ihdr);
                        repaired[crc_start..crc_start + 4]
                            .copy_from_slice(&crc32(&crc_input).to_be_bytes());
                    }
                    let _ = decode_png(&repaired);
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // PPM / PGM tests
    // ---------------------------------------------------------------------

    fn pnm(magic: u8, header: &[u8], raster: &[u8]) -> Vec<u8> {
        let mut out = vec![b'P', magic];
        out.extend_from_slice(header);
        out.extend_from_slice(raster);
        out
    }

    #[test]
    fn decodes_ppm_p6() {
        let file = pnm(b'6', b"\n2 1\n255\n", &[1, 2, 3, 4, 5, 6]);
        assert_eq!(detect_format(&file), Some(ImageFormat::Pnm));
        let image = decode(&file).expect("P6 should decode");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.get(0, 0), [1, 2, 3, 255]);
        assert_eq!(image.get(1, 0), [4, 5, 6, 255]);
    }

    #[test]
    fn decodes_pgm_p5_with_comments_and_scaling() {
        // Comments before and between tokens; with maxval 15 samples scale by 17.
        let file = pnm(b'5', b"\n# a comment\n2 # width done\n1\n15\n", &[0, 15]);
        let image = decode(&file).expect("P5 should decode");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.get(0, 0), [0, 0, 0, 255]);
        assert_eq!(image.get(1, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn decodes_16_bit_ppm() {
        // maxval 1023 selects two big-endian bytes per sample.
        let file = pnm(b'6', b" 1 1 1023\n", &[0x03, 0xe8, 0, 100, 0x03, 0xff]);
        let image = decode(&file).expect("16-bit P6 should decode");
        assert_eq!(image.get(0, 0), [249, 24, 255, 255]);
    }

    #[test]
    fn rejects_malformed_pnm() {
        // maxval out of range, truncated raster, zero dimension.
        assert!(decode(b"P6\n1 1\n70000\n\x00\x00\x00").is_err());
        assert!(decode(b"P6\n2 1\n255\n\x01\x02\x03").is_err());
        assert!(decode(b"P5\n0 1\n255\n").is_err());
        // Missing whitespace between the header and the raster.
        assert!(decode(b"P6\n1 1\n255X\x00\x00\x00").is_err());
        // A sample larger than maxval.
        assert!(decode(b"P5\n1 1\n15\n\xff").is_err());

        let oversized = pnm(
            b'5',
            format!("\n{} 1\n255\n", MAX_DIMENSION + 1).as_bytes(),
            &[0],
        );
        let error = decode(&oversized).expect_err("oversized PNM must be rejected");
        assert!(error.message.contains("exceed"), "{}", error.message);
    }

    // ---------------------------------------------------------------------
    // ICO tests
    // ---------------------------------------------------------------------

    fn ico_with_png(png: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0, 0, 1, 0]); // reserved = 0, type = icon
        out.extend_from_slice(&1_u16.to_le_bytes()); // one directory entry
        out.extend_from_slice(&[0, 0, 0, 0]); // width/height (0 = 256), colours, reserved
        out.extend_from_slice(&1_u16.to_le_bytes()); // planes
        out.extend_from_slice(&32_u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes()); // size
        out.extend_from_slice(&22_u32.to_le_bytes()); // image offset (6 + 16)
        out.extend_from_slice(png);
        out
    }

    #[test]
    fn decodes_ico_wrapping_png() {
        let rgba: Vec<u8> = (0..16_u8)
            .flat_map(|index| [index * 4, index * 4 + 1, index * 4 + 2, 255])
            .collect();
        let png = png::encode(4, 4, &rgba);
        let file = ico_with_png(&png);
        assert_eq!(detect_format(&file), Some(ImageFormat::Ico));
        let image = decode(&file).expect("ICO-wrapped PNG should decode");
        assert_eq!((image.width, image.height), (4, 4));
        assert_eq!(image.pixels, rgba);
    }

    #[test]
    fn rejects_malformed_icos() {
        assert!(decode(&[0, 0, 1, 0]).is_err()); // truncated header
        assert!(decode(&[0, 0, 2, 0, 1, 0]).is_err()); // CUR, not ICO
        assert!(decode(&[0, 0, 1, 0, 0, 0]).is_err()); // no entries
        assert!(decode(&[0, 0, 1, 0, 1, 0]).is_err()); // truncated directory

        // An entry holding a DIB rather than PNG is reported as unsupported.
        let mut file = ico_with_png(&png::encode(1, 1, &[1, 2, 3, 255]));
        file[22] = 0x42; // break the embedded PNG signature
        let error = decode(&file).expect_err("non-PNG ICO entry must be rejected");
        assert!(error.message.contains("unsupported"), "{}", error.message);

        // An entry pointing outside the file is rejected.
        let mut file = ico_with_png(&png::encode(1, 1, &[1, 2, 3, 255]));
        file[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&file).is_err());
    }

    // ---------------------------------------------------------------------
    // Detected-but-unsupported formats
    // ---------------------------------------------------------------------

    #[test]
    fn detects_avif_and_rasterizes_svg() {
        let avif = b"\x00\x00\x00\x18ftypavif\x00\x00\x00\x00avifmif1";
        assert_eq!(detect_format(avif), Some(ImageFormat::Avif));
        let error = decode(avif).expect_err("AVIF must be reported as unsupported");
        assert!(error.message.contains("AVIF"), "{}", error.message);

        let avis = b"\x00\x00\x00\x18ftypavis\x00\x00\x00\x00avismif1";
        assert_eq!(detect_format(avis), Some(ImageFormat::Avif));

        for svg in [
            &b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"[..],
            &b"\xef\xbb\xbf<?xml version=\"1.0\"?><svg></svg>"[..],
            &b"  <!-- c --><svg></svg>"[..],
        ] {
            assert_eq!(detect_format(svg), Some(ImageFormat::Svg));
            // Dimension-less documents are still valid SVG bytes; the rasterizer
            // reports the missing dimensions rather than "unsupported format".
            assert!(decode(svg).is_err());
        }
    }

    #[test]
    fn decodes_svg_bytes_into_non_empty_pixels() {
        let svg = b"<svg width='8' height='8'>\
                      <rect width='8' height='8' fill='#336699'/>\
                    </svg>";
        assert_eq!(detect_format(svg), Some(ImageFormat::Svg));
        let image = decode(svg).expect("SVG should be rasterized");
        assert_eq!((image.width, image.height), (8, 8));
        assert_eq!(image.pixels.len(), 8 * 8 * 4);
        assert!(image.pixels.iter().any(|channel| *channel != 0));
        assert_eq!(image.get(4, 4), [0x33, 0x66, 0x99, 255]);

        // The explicit SVG entry point works for payloads whose format is known
        // from a `.svg` URL even if byte sniffing is skipped.
        let explicit = decode_svg(svg).expect("explicit SVG decode");
        assert_eq!(explicit, image);
    }

    // ---------------------------------------------------------------------
    // JPEG test helpers — a tiny baseline encoder used to build fixtures
    // ---------------------------------------------------------------------

    /// Writes bits most-significant bit first, stuffing `ff 00` for data bytes
    /// equal to `0xff`. Markers are emitted separately so they are not stuffed.
    struct JpegBitWriter {
        out: Vec<u8>,
        accumulator: u32,
        bits: u32,
    }

    impl JpegBitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                accumulator: 0,
                bits: 0,
            }
        }

        fn write(&mut self, value: u32, width: u32) {
            for bit in (0..width).rev() {
                self.accumulator = (self.accumulator << 1) | ((value >> bit) & 1);
                self.bits += 1;
                if self.bits == 8 {
                    let byte = self.accumulator as u8;
                    self.out.push(byte);
                    if byte == 0xff {
                        self.out.push(0x00);
                    }
                    self.accumulator = 0;
                    self.bits = 0;
                }
            }
        }

        fn flush(&mut self) {
            if self.bits > 0 {
                let padding = 8 - self.bits;
                self.accumulator = (self.accumulator << padding) | ((1_u32 << padding) - 1);
                let byte = self.accumulator as u8;
                self.out.push(byte);
                if byte == 0xff {
                    self.out.push(0x00);
                }
                self.accumulator = 0;
                self.bits = 0;
            }
        }

        fn marker(&mut self, marker: u8) {
            self.flush();
            self.out.extend_from_slice(&[0xff, marker]);
        }

        fn finish(mut self) -> Vec<u8> {
            self.flush();
            self.out
        }
    }

    fn jpeg_segment(marker: u8, payload: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&[0xff, marker]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
    }

    fn jpeg_bit_length(value: i32) -> u32 {
        32 - value.unsigned_abs().leading_zeros()
    }

    fn jpeg_magnitude(value: i32, bits: u32) -> u32 {
        if value > 0 {
            value as u32
        } else {
            (value + (1_i32 << bits) - 1) as u32
        }
    }

    /// DC uses 16 symbols of length 4 (code == symbol); AC uses `symbols` of
    /// length 8 (code == position in the list).
    fn jpeg_ac_code(symbols: &[u8], symbol: u8) -> u32 {
        symbols
            .iter()
            .position(|&candidate| candidate == symbol)
            .expect("AC symbol must be present in the table") as u32
    }

    fn jpeg_encode_diff(writer: &mut JpegBitWriter, diff: i32) {
        let bits = jpeg_bit_length(diff);
        if bits == 0 {
            writer.write(0, 4);
            return;
        }
        writer.write(bits, 4);
        writer.write(jpeg_magnitude(diff, bits), bits);
    }

    /// Builds a complete baseline JPEG. `mcu_blocks[mcu][component][block]` holds
    /// 64 quantized coefficients in zig-zag order.
    #[allow(clippy::too_many_arguments)]
    fn jpeg_build(
        width: u16,
        height: u16,
        components: &[(u8, u8, u8)], // id, h, v (quantization table 0)
        quant: &[i32; 64],           // zig-zag order
        ac_symbols: &[u8],
        restart_interval: u16,
        mcu_blocks: &[Vec<Vec<[i32; 64]>>],
    ) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];

        let mut dqt = vec![0_u8; 65];
        for (index, &value) in quant.iter().enumerate() {
            dqt[1 + index] = value.clamp(1, 255) as u8;
        }
        jpeg_segment(0xdb, &dqt, &mut out);

        let mut sof = vec![8_u8];
        sof.extend_from_slice(&height.to_be_bytes());
        sof.extend_from_slice(&width.to_be_bytes());
        sof.push(components.len() as u8);
        for &(id, h, v) in components {
            sof.extend_from_slice(&[id, (h << 4) | v, 0]);
        }
        jpeg_segment(0xc0, &sof, &mut out);

        let mut dht_dc = vec![0_u8; 33];
        dht_dc[4] = 16; // 16 codes of length 4
        for (index, symbol) in dht_dc[17..].iter_mut().enumerate() {
            *symbol = index as u8;
        }
        jpeg_segment(0xc4, &dht_dc, &mut out);

        let mut dht_ac = vec![0_u8; 17 + ac_symbols.len()];
        dht_ac[0] = 0x10; // AC table 0
        dht_ac[8] = ac_symbols.len() as u8; // codes of length 8
        dht_ac[17..].copy_from_slice(ac_symbols);
        jpeg_segment(0xc4, &dht_ac, &mut out);

        if restart_interval > 0 {
            jpeg_segment(0xdd, &restart_interval.to_be_bytes(), &mut out);
        }

        let mut sos = vec![components.len() as u8];
        for &(id, _, _) in components {
            sos.extend_from_slice(&[id, 0x00]); // DC table 0, AC table 0
        }
        sos.extend_from_slice(&[0, 63, 0]);
        jpeg_segment(0xda, &sos, &mut out);

        let mut writer = JpegBitWriter::new();
        let mut predictors = vec![0_i32; components.len()];
        let mut restart_index = 0_u8;
        for (mcu_index, blocks) in mcu_blocks.iter().enumerate() {
            if restart_interval > 0
                && mcu_index > 0
                && mcu_index % usize::from(restart_interval) == 0
            {
                writer.marker(0xd0 + restart_index);
                restart_index = (restart_index + 1) % 8;
                predictors.fill(0);
            }
            for (component_index, component_blocks) in blocks.iter().enumerate() {
                for coefficients in component_blocks {
                    jpeg_encode_diff(&mut writer, coefficients[0] - predictors[component_index]);
                    predictors[component_index] = coefficients[0];
                    let mut zero_run = 0_usize;
                    for &value in coefficients.iter().skip(1) {
                        if value == 0 {
                            zero_run += 1;
                            continue;
                        }
                        while zero_run > 15 {
                            writer.write(jpeg_ac_code(ac_symbols, 0xf0), 8);
                            zero_run -= 16;
                        }
                        let bits = jpeg_bit_length(value);
                        let symbol = ((zero_run as u8) << 4) | bits as u8;
                        writer.write(jpeg_ac_code(ac_symbols, symbol), 8);
                        writer.write(jpeg_magnitude(value, bits), bits);
                        zero_run = 0;
                    }
                    if zero_run > 0 {
                        writer.write(jpeg_ac_code(ac_symbols, 0x00), 8);
                    }
                }
            }
        }
        out.extend_from_slice(&writer.finish());
        out.extend_from_slice(&[0xff, 0xd9]);
        out
    }

    fn dc_block(value: i32) -> [i32; 64] {
        let mut block = [0_i32; 64];
        block[0] = value;
        block
    }

    #[test]
    fn decodes_grayscale_baseline_jpeg() {
        let quant = [1_i32; 64];
        let file = jpeg_build(
            8,
            8,
            &[(1, 1, 1)],
            &quant,
            &[0x00],
            0,
            &[vec![vec![dc_block(8)]]],
        );
        assert_eq!(detect_format(&file), Some(ImageFormat::Jpeg));
        let image = decode(&file).expect("greyscale JPEG should decode");
        assert_eq!((image.width, image.height), (8, 8));
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(image.get(x, y), [129, 129, 129, 255], "at {x},{y}");
            }
        }
        assert_eq!(ImageData::decode(&file).expect("convenience decode"), image);
    }

    #[test]
    fn decodes_ac_coefficient_in_baseline_jpeg() {
        // A single F(0, 1) coefficient produces a horizontal cosine ramp.
        let mut block = [0_i32; 64];
        block[1] = 64;
        let file = jpeg_build(
            8,
            8,
            &[(1, 1, 1)],
            &[1_i32; 64],
            &[0x00, 0x07],
            0,
            &[vec![vec![block]]],
        );
        let image = decode(&file).expect("AC coefficient should decode");
        let alpha = 1.0 / 2.0_f32.sqrt();
        for x in 0..8_u32 {
            let cosine = ((2 * x + 1) as f32 * std::f32::consts::PI / 16.0).cos();
            let expected = (128.0 + 0.25 * 64.0 * cosine * alpha).round() as u8;
            for y in 0..8 {
                assert_eq!(
                    image.get(x, y),
                    [expected, expected, expected, 255],
                    "at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn decodes_444_baseline_jpeg() {
        // Y=100, Cb=150, Cr=200, all flat.
        let file = jpeg_build(
            8,
            8,
            &[(1, 1, 1), (2, 1, 1), (3, 1, 1)],
            &[1_i32; 64],
            &[0x00],
            0,
            &[vec![
                vec![dc_block(-224)],
                vec![dc_block(176)],
                vec![dc_block(576)],
            ]],
        );
        let image = decode(&file).expect("4:4:4 JPEG should decode");
        let y = 100_f32;
        let cb = 22_f32;
        let cr = 72_f32;
        let expected = [
            (y + 1.402 * cr).round().clamp(0.0, 255.0) as u8,
            (y - 0.344_136 * cb - 0.714_136 * cr)
                .round()
                .clamp(0.0, 255.0) as u8,
            (y + 1.772 * cb).round().clamp(0.0, 255.0) as u8,
            255,
        ];
        for row in 0..8 {
            for column in 0..8 {
                assert_eq!(image.get(column, row), expected, "at {column},{row}");
            }
        }
    }

    #[test]
    fn decodes_422_subsampled_jpeg() {
        // One 16x8 MCU: Y has two horizontal blocks (128 then 138), chroma is
        // flat grey. Nearest-neighbour upsampling must map x=0..8 to the first
        // block and x=8..16 to the second.
        let file = jpeg_build(
            16,
            8,
            &[(1, 2, 1), (2, 1, 1), (3, 1, 1)],
            &[1_i32; 64],
            &[0x00],
            0,
            &[vec![
                vec![dc_block(0), dc_block(80)],
                vec![dc_block(0)],
                vec![dc_block(0)],
            ]],
        );
        let image = decode(&file).expect("4:2:2 JPEG should decode");
        assert_eq!((image.width, image.height), (16, 8));
        for x in 0..16_u32 {
            let value = if x < 8 { 128 } else { 138 };
            for y in 0..8 {
                assert_eq!(image.get(x, y), [value, value, value, 255], "at {x},{y}");
            }
        }
    }

    #[test]
    fn decodes_420_subsampled_jpeg() {
        // One 16x16 MCU: the Y plane is four 8x8 blocks. Only the top-left
        // block differs, so upsampling must confine it to the top-left quadrant.
        let file = jpeg_build(
            16,
            16,
            &[(1, 2, 2), (2, 1, 1), (3, 1, 1)],
            &[1_i32; 64],
            &[0x00],
            0,
            &[vec![
                vec![dc_block(0), dc_block(80), dc_block(80), dc_block(80)],
                vec![dc_block(0)],
                vec![dc_block(0)],
            ]],
        );
        let image = decode(&file).expect("4:2:0 JPEG should decode");
        assert_eq!((image.width, image.height), (16, 16));
        for y in 0..16_u32 {
            for x in 0..16_u32 {
                let value = if x < 8 && y < 8 { 128 } else { 138 };
                assert_eq!(image.get(x, y), [value, value, value, 255], "at {x},{y}");
            }
        }
    }

    #[test]
    fn decodes_jpeg_with_restart_interval() {
        // Four MCUs, a restart after each. Absolute DC values are chosen so
        // that missing the predictor reset would decode the wrong rows.
        let mcus: Vec<Vec<Vec<[i32; 64]>>> = [0, 80, 160, 240]
            .iter()
            .map(|&dc| vec![vec![dc_block(dc)]])
            .collect();
        let file = jpeg_build(8, 32, &[(1, 1, 1)], &[1_i32; 64], &[0x00], 1, &mcus);
        let image = decode(&file).expect("JPEG with restarts should decode");
        for (mcu, &value) in [128_u8, 138, 148, 158].iter().enumerate() {
            for y in (mcu * 8) as u32..((mcu + 1) * 8) as u32 {
                for x in 0..8 {
                    assert_eq!(image.get(x, y), [value, value, value, 255], "at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn rejects_progressive_and_other_non_baseline_jpegs() {
        let base = jpeg_build(
            8,
            8,
            &[(1, 1, 1)],
            &[1_i32; 64],
            &[0x00],
            0,
            &[vec![vec![dc_block(0)]]],
        );
        let sof = base
            .windows(2)
            .position(|window| window == [0xff, 0xc0])
            .expect("SOF0 marker");

        // SOF2: progressive DCT.
        let mut progressive = base.clone();
        progressive[sof + 1] = 0xc2;
        let error = decode(&progressive).expect_err("progressive JPEG must be rejected");
        assert!(error.message.contains("progressive"), "{}", error.message);
        assert!(error.message.contains("unsupported"), "{}", error.message);

        // SOF1: extended sequential DCT.
        let mut extended = base.clone();
        extended[sof + 1] = 0xc1;
        let error = decode(&extended).expect_err("extended JPEG must be rejected");
        assert!(error.message.contains("unsupported"), "{}", error.message);

        // 12-bit precision in SOF0.
        let mut deep = base.clone();
        deep[sof + 4] = 12;
        let error = decode(&deep).expect_err("12-bit JPEG must be rejected");
        assert!(error.message.contains("unsupported"), "{}", error.message);
    }

    #[test]
    fn rejects_oversized_jpeg_dimensions() {
        let mut file = jpeg_build(
            8,
            8,
            &[(1, 1, 1)],
            &[1_i32; 64],
            &[0x00],
            0,
            &[vec![vec![dc_block(0)]]],
        );
        let sof = file
            .windows(2)
            .position(|window| window == [0xff, 0xc0])
            .expect("SOF0 marker");
        let width_offset = sof + 4 + 3;
        file[width_offset..width_offset + 2].copy_from_slice(&5000_u16.to_be_bytes());
        let error = decode(&file).expect_err("oversized JPEG must be rejected");
        assert!(error.message.contains("exceed"), "{}", error.message);
    }

    #[test]
    fn jpeg_never_panics_on_random_or_mutated_data() {
        let valid = jpeg_build(
            8,
            8,
            &[(1, 1, 1)],
            &[1_i32; 64],
            &[0x00],
            0,
            &[vec![vec![dc_block(0)]]],
        );
        let mut state = 0xdead_beef_1234_5678_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        };
        for _ in 0..400 {
            let mut bytes = vec![0xff, 0xd8];
            for _ in 0..64 {
                bytes.push(next());
            }
            let _ = decode(&bytes);

            let mut file = valid.clone();
            let mutations = 1 + (next() as usize % 5);
            for _ in 0..mutations {
                let index = (next() as usize) % file.len();
                file[index] ^= next();
            }
            let _ = decode(&file);
            let cut = (next() as usize) % file.len();
            let _ = decode(&file[..cut]);
        }
    }
}
