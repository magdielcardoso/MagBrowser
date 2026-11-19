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
//! * **WebP** — RIFF/WebP containers with a `VP8L` lossless bitstream (all four
//!   reversible transforms, colour cache, LZ77 backward references and
//!   spatially varying Huffman codes) or a `VP8 ` lossy key frame (all intra
//!   prediction modes, token probabilities, dequantized DCT/WHT, loop filter
//!   and BT.601 YCbCr conversion). Extended-format `VP8X` files with an `ALPH`
//!   alpha chunk are supported. Animated WebP, non-key frames and frames whose
//!   canvas differs from the bitstream dimensions are reported as unsupported.
//!
//! # Unsupported formats
//!
//! AVIF is detected and reported as unsupported with a [`DecodeError`], never
//! silently decoded as garbage. Progressive and other non-baseline JPEG modes
//! are likewise reported as unsupported. Animated GIF frames after the first,
//! APNG animation, and 16-bit PNG samples are not decoded. [`detect_format`]
//! lets callers choose a fallback without attempting a decode.

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
        Some(ImageFormat::Webp) => decode_webp(bytes),
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

// ---------------------------------------------------------------------------
// WebP (RIFF container with VP8L / VP8 bitstreams)
// ---------------------------------------------------------------------------
//
// The decoders below are intentionally self-contained. VP8L uses an LSB-first
// bit reader, canonical Huffman codes, an LZ77 backward-reference window, a
// small colour cache and four reversible image transforms. VP8 decodes single
// key frames (the only frame type WebP uses for still images): the boolean
// entropy coder, all intra prediction modes, token probabilities, dequantized
// 4x4 DCT/WHT, and the loop filter. Unsupported features produce a typed
// DecodeError; no code path panics on hostile input.

fn webp_err(message: impl Into<String>) -> DecodeError {
    DecodeError::new(format!("WebP: {}", message.into()))
}

fn webp_unsupported(message: impl Into<String>) -> DecodeError {
    DecodeError::unsupported(format!("WebP {}", message.into()))
}

fn webp_u16le(bytes: &[u8], offset: usize) -> Result<u16, DecodeError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| webp_err("truncated RIFF chunk"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn webp_u24le(bytes: &[u8], offset: usize) -> Result<u32, DecodeError> {
    let slice = bytes
        .get(offset..offset + 3)
        .ok_or_else(|| webp_err("truncated RIFF chunk"))?;
    Ok(u32::from(slice[0]) | (u32::from(slice[1]) << 8) | (u32::from(slice[2]) << 16))
}

fn webp_u32le(bytes: &[u8], offset: usize) -> Result<u32, DecodeError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| webp_err("truncated RIFF chunk"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn div_round_up(value: usize, divisor: usize) -> Result<usize, DecodeError> {
    if divisor == 0 {
        return Err(webp_err("zero divisor in subsampled image size"));
    }
    Ok(value.div_ceil(divisor))
}

// -------------------------------------------------------------------------
// VP8L (lossless)
// -------------------------------------------------------------------------

/// Least-significant-bit-first reader for VP8L entropy-coded data.
struct Vp8LReader<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u64,
    bits: u32,
}

impl<'a> Vp8LReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buffer: 0,
            bits: 0,
        }
    }

    fn read_bits(&mut self, count: u32) -> Result<u32, DecodeError> {
        if count > 32 {
            return Err(webp_err("VP8L read exceeds 32 bits"));
        }
        while self.bits < count {
            let byte = *self
                .data
                .get(self.pos)
                .ok_or_else(|| webp_err("truncated VP8L bitstream"))?;
            self.pos += 1;
            self.buffer |= u64::from(byte) << self.bits;
            self.bits += 8;
        }
        let mask = if count == 0 { 0 } else { (1_u64 << count) - 1 };
        let value = (self.buffer & mask) as u32;
        self.buffer >>= count;
        self.bits -= count;
        Ok(value)
    }

    fn read_bit(&mut self) -> Result<u32, DecodeError> {
        self.read_bits(1)
    }
}

/// Canonical prefix code. A tree with a single symbol consumes no bits when
/// decoded, matching libwebp's `VP8LBuildHuffmanTable` special case.
struct Vp8LHuffman {
    single: Option<usize>,
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Vp8LHuffman {
    fn from_lengths(lengths: &[u8]) -> Result<Self, DecodeError> {
        let mut counts = [0_u16; 16];
        let mut total = 0_usize;
        let mut single = None;
        for (symbol, &length) in lengths.iter().enumerate() {
            if length > 15 {
                return Err(webp_err("invalid VP8L Huffman code length"));
            }
            if length > 0 {
                counts[usize::from(length)] += 1;
                total += 1;
                single = Some(symbol);
            }
        }
        // libwebp requires a full binary tree (apart from the single-leaf
        // special case). Reject over-subscribed sets; incomplete sets are
        // harmless for a decoder that simply refuses unknown codes.
        let mut left: i32 = 1;
        for &count in counts.iter().skip(1) {
            left = left * 2 - i32::from(count);
            if left < 0 {
                return Err(webp_err("over-subscribed VP8L Huffman code"));
            }
        }
        if total == 1 {
            return Ok(Self {
                single,
                counts,
                symbols: Vec::new(),
            });
        }
        let mut offsets = [0_u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0_u16; total];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length > 0 {
                let slot = usize::from(offsets[usize::from(length)]);
                if slot >= symbols.len() {
                    return Err(webp_err("over-subscribed VP8L Huffman code"));
                }
                symbols[slot] = u16::try_from(symbol)
                    .map_err(|_| webp_err("VP8L Huffman symbol out of range"))?;
                offsets[usize::from(length)] += 1;
            }
        }
        Ok(Self {
            single: None,
            counts,
            symbols,
        })
    }

    fn decode(&self, reader: &mut Vp8LReader<'_>) -> Result<usize, DecodeError> {
        if let Some(symbol) = self.single {
            return Ok(symbol);
        }
        let mut code = 0_u32;
        let mut first = 0_u32;
        let mut index = 0_usize;
        for length in 1..=15 {
            code = (code << 1) | reader.read_bit()?;
            let count = u32::from(self.counts[length]);
            if code >= first && code - first < count {
                return self
                    .symbols
                    .get(index + (code - first) as usize)
                    .map(|&symbol| usize::from(symbol))
                    .ok_or_else(|| webp_err("invalid VP8L Huffman code"));
            }
            index += count as usize;
            first = (first + count) << 1;
        }
        Err(webp_err("invalid VP8L Huffman code"))
    }
}

const VP8L_CODE_LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

/// Reads one of the five prefix codes of a Huffman group (or a 19-symbol code
/// length alphabet when `alphabet_size == 19` and this is used recursively).
fn vp8l_read_huffman(
    reader: &mut Vp8LReader<'_>,
    alphabet_size: usize,
) -> Result<Vp8LHuffman, DecodeError> {
    let mut lengths = vec![0_u8; alphabet_size];
    if reader.read_bit()? == 1 {
        // Simple code length code: one or two literal symbols of length one.
        let num_symbols = reader.read_bit()? as usize + 1;
        let first_is_wide = reader.read_bit()?;
        let bits = if first_is_wide == 1 { 8 } else { 1 };
        let first = reader.read_bits(bits)? as usize;
        // libwebp permits a simple-code symbol beyond this alphabet (the
        // code is then effectively empty). Such a code is only valid if no
        // symbol is ever decoded from it, so simply ignore the out-of-range
        // length here.
        if let Some(length) = lengths.get_mut(first) {
            *length = 1;
        }
        if num_symbols == 2 {
            let second = reader.read_bits(8)? as usize;
            if let Some(length) = lengths.get_mut(second) {
                *length = 1;
            }
        }
        return Vp8LHuffman::from_lengths(&lengths);
    }

    let num_codes = reader.read_bits(4)? as usize + 4;
    let mut code_length_lengths = [0_u8; 19];
    for &slot in VP8L_CODE_LENGTH_ORDER.iter().take(num_codes) {
        code_length_lengths[slot] = reader.read_bits(3)? as u8;
    }
    let code_length_code = Vp8LHuffman::from_lengths(&code_length_lengths)?;

    let max_symbol = if reader.read_bit()? == 1 {
        let length_nbits = 2 + 2 * reader.read_bits(3)?;
        let max = 2 + reader.read_bits(length_nbits)? as usize;
        if max > alphabet_size {
            return Err(webp_err("VP8L max symbol exceeds alphabet"));
        }
        max
    } else {
        alphabet_size
    };

    let mut index = 0_usize;
    let mut previous_length = 8_u8;
    while index < max_symbol {
        let symbol = code_length_code.decode(reader)?;
        match symbol {
            0..=15 => {
                lengths[index] = symbol as u8;
                if symbol != 0 {
                    previous_length = symbol as u8;
                }
                index += 1;
            }
            16..=18 => {
                let (extra_bits, offset) = match symbol {
                    16 => (2_u32, 3_usize),
                    17 => (3, 3),
                    _ => (7, 11),
                };
                let repeat = reader.read_bits(extra_bits)? as usize + offset;
                let end = index
                    .checked_add(repeat)
                    .ok_or_else(|| webp_err("VP8L code length repeat overflow"))?;
                if end > alphabet_size {
                    return Err(webp_err("VP8L code length repeat overruns alphabet"));
                }
                let value = if symbol == 16 { previous_length } else { 0 };
                for slot in &mut lengths[index..end] {
                    *slot = value;
                }
                index = end;
            }
            _ => return Err(webp_err("invalid VP8L code length symbol")),
        }
    }
    Vp8LHuffman::from_lengths(&lengths)
}

/// Decodes a VP8L "prefix code + extra bits" integer (used for LZ77 lengths and
/// distances).
fn vp8l_lex(reader: &mut Vp8LReader<'_>, prefix: usize) -> Result<usize, DecodeError> {
    if prefix < 4 {
        return Ok(prefix + 1);
    }
    let extra_bits = ((prefix - 2) >> 1) as u32;
    let offset = (2 + (prefix & 1)) << extra_bits;
    Ok(offset + reader.read_bits(extra_bits)? as usize + 1)
}

/// Compact form of the 120 special (x, y) neighbours for VP8L distances.
const VP8L_CODE_TO_PLANE: [u8; 120] = [
    0x18, 0x07, 0x17, 0x19, 0x28, 0x06, 0x27, 0x29, 0x16, 0x1a, 0x26, 0x2a, 0x38, 0x05, 0x37, 0x39,
    0x15, 0x1b, 0x36, 0x3a, 0x25, 0x2b, 0x48, 0x04, 0x47, 0x49, 0x14, 0x1c, 0x35, 0x3b, 0x46, 0x4a,
    0x24, 0x2c, 0x58, 0x45, 0x4b, 0x34, 0x3c, 0x03, 0x57, 0x59, 0x13, 0x1d, 0x56, 0x5a, 0x23, 0x2d,
    0x44, 0x4c, 0x55, 0x5b, 0x33, 0x3d, 0x68, 0x02, 0x67, 0x69, 0x12, 0x1e, 0x66, 0x6a, 0x22, 0x2e,
    0x54, 0x5c, 0x43, 0x4d, 0x65, 0x6b, 0x32, 0x3e, 0x78, 0x01, 0x77, 0x79, 0x53, 0x5d, 0x11, 0x1f,
    0x64, 0x6c, 0x42, 0x4e, 0x76, 0x7a, 0x21, 0x2f, 0x75, 0x7b, 0x31, 0x3f, 0x63, 0x6d, 0x52, 0x5e,
    0x00, 0x74, 0x7c, 0x41, 0x4f, 0x10, 0x20, 0x62, 0x6e, 0x30, 0x73, 0x7d, 0x51, 0x5f, 0x40, 0x72,
    0x7e, 0x61, 0x6f, 0x50, 0x71, 0x7f, 0x60, 0x70,
];

fn vp8l_distance(
    reader: &mut Vp8LReader<'_>,
    distance_symbol: usize,
    width: usize,
) -> Result<usize, DecodeError> {
    let code = vp8l_lex(reader, distance_symbol)?;
    if code == 0 {
        return Err(webp_err("invalid VP8L distance code"));
    }
    if code > 120 {
        return Ok(code - 120);
    }
    let plane = *VP8L_CODE_TO_PLANE
        .get(code - 1)
        .ok_or_else(|| webp_err("VP8L distance code out of range"))?;
    let y_offset = i64::from(plane >> 4);
    let x_offset = i64::from(8 - (plane & 0x0f));
    let dist = y_offset * width as i64 + x_offset;
    Ok(if dist < 1 { 1 } else { dist as usize })
}

/// One reversible transform applied to a VP8L image.
enum Vp8LTransform {
    Predictor {
        size_bits: u32,
        transform_width: usize,
        modes: Vec<u8>,
    },
    Color {
        size_bits: u32,
        transform_width: usize,
        /// [red_to_blue, green_to_blue, green_to_red] per block.
        elements: Vec<[u8; 3]>,
    },
    SubtractGreen,
    ColorIndex {
        table: Vec<[u8; 4]>,
        width_bits: u32,
        input_width: usize,
        output_width: usize,
    },
}

/// Decodes one VP8L image stream. `level0` is true for the main image, which
/// may carry transforms and multiple Huffman groups; sub-images (predictor,
/// colour transform, palette, entropy image, ALPH data) have neither.
/// Pixels are returned as ARGB quadruplets `[A, R, G, B]`.
fn vp8l_decode_stream(
    reader: &mut Vp8LReader<'_>,
    width: usize,
    height: usize,
    level0: bool,
) -> Result<Vec<[u8; 4]>, DecodeError> {
    if width == 0 || height == 0 {
        return Err(webp_err("VP8L image has a zero dimension"));
    }
    check_pixel_count(
        u32::try_from(width).map_err(|_| webp_err("VP8L image too wide"))?,
        u32::try_from(height).map_err(|_| webp_err("VP8L image too tall"))?,
        "VP8L",
    )?;

    let mut transforms: Vec<Vp8LTransform> = Vec::new();
    let mut current_width = width;

    if level0 {
        while reader.read_bit()? == 1 {
            let transform_type = reader.read_bits(2)?;
            match transform_type {
                0 | 1 => {
                    let size_bits = reader.read_bits(3)? + 2;
                    let block = 1_usize << size_bits;
                    let transform_width = div_round_up(current_width, block)?;
                    let transform_height = div_round_up(height, block)?;
                    let image =
                        vp8l_decode_stream(reader, transform_width, transform_height, false)?;
                    if transform_type == 0 {
                        let modes: Vec<u8> = image.iter().map(|pixel| pixel[2]).collect();
                        transforms.push(Vp8LTransform::Predictor {
                            size_bits,
                            transform_width,
                            modes,
                        });
                    } else {
                        let elements: Vec<[u8; 3]> = image
                            .iter()
                            .map(|pixel| [pixel[1], pixel[2], pixel[3]])
                            .collect();
                        transforms.push(Vp8LTransform::Color {
                            size_bits,
                            transform_width,
                            elements,
                        });
                    }
                }
                2 => transforms.push(Vp8LTransform::SubtractGreen),
                3 => {
                    let table_size = reader.read_bits(8)? as usize + 1;
                    let image = vp8l_decode_stream(reader, table_size, 1, false)?;
                    let mut table = Vec::with_capacity(table_size);
                    let mut previous = [0_u8; 4];
                    for delta in image {
                        let color = [
                            previous[0].wrapping_add(delta[0]),
                            previous[1].wrapping_add(delta[1]),
                            previous[2].wrapping_add(delta[2]),
                            previous[3].wrapping_add(delta[3]),
                        ];
                        table.push(color);
                        previous = color;
                    }
                    let width_bits = if table_size <= 2 {
                        3_u32
                    } else if table_size <= 4 {
                        2
                    } else if table_size <= 16 {
                        1
                    } else {
                        0
                    };
                    let output_width = current_width;
                    current_width = div_round_up(current_width, 1_usize << width_bits)?;
                    transforms.push(Vp8LTransform::ColorIndex {
                        table,
                        width_bits,
                        input_width: current_width,
                        output_width,
                    });
                }
                _ => return Err(webp_err("invalid VP8L transform type")),
            }
        }
    }

    // Colour cache. Its symbols only extend the green alphabet.
    let (cache_bits, cache_size) = if reader.read_bit()? == 1 {
        let bits = reader.read_bits(4)?;
        if bits > 11 {
            return Err(webp_err("invalid VP8L colour cache size"));
        }
        (bits, 1_usize << bits)
    } else {
        (0, 0_usize)
    };
    let mut color_cache = vec![[0_u8; 4]; cache_size];

    // Meta Huffman codes (main image only).
    let mut meta_image: Option<Vec<usize>> = None;
    let mut meta_bits = 0_u32;
    let mut meta_width = 0_usize;
    let mut num_groups = 1_usize;
    if level0 && reader.read_bit()? == 1 {
        meta_bits = reader.read_bits(3)? + 2;
        let block = 1_usize << meta_bits;
        meta_width = div_round_up(current_width, block)?;
        let meta_height = div_round_up(height, block)?;
        let image = vp8l_decode_stream(reader, meta_width, meta_height, false)?;
        let mut groups: Vec<usize> = image
            .iter()
            .map(|pixel| (usize::from(pixel[1]) << 8) | usize::from(pixel[2]))
            .collect();
        let max_group = groups.iter().copied().max().unwrap_or(0);
        if max_group > 65_535 {
            return Err(webp_err("VP8L meta prefix code out of range"));
        }
        // Remap sparse group numbers to a dense range so a hostile stream
        // cannot force a huge allocation.
        let mut mapping = vec![usize::MAX; max_group + 1];
        num_groups = 0;
        for group in &mut groups {
            if mapping[*group] == usize::MAX {
                mapping[*group] = num_groups;
                num_groups += 1;
            }
            *group = mapping[*group];
        }
        if num_groups > 1 {
            meta_image = Some(groups);
        } else {
            meta_bits = 0;
            meta_width = 0;
            num_groups = 1;
        }
    }

    // Five codes per group: green (+ length + cache), red, blue, alpha, dist.
    let alphabets = [256 + 24 + cache_size, 256, 256, 256, 40];
    let mut groups: Vec<Vec<Vp8LHuffman>> = Vec::with_capacity(num_groups);
    for _ in 0..num_groups {
        let mut codes = Vec::with_capacity(5);
        for &alphabet in &alphabets {
            codes.push(vp8l_read_huffman(reader, alphabet)?);
        }
        groups.push(codes);
    }

    let pixel_count = current_width
        .checked_mul(height)
        .ok_or_else(|| webp_err("VP8L image size overflow"))?;
    let mut pixels = vec![[0_u8; 4]; pixel_count];
    let mut index = 0_usize;
    while index < pixel_count {
        let group = match &meta_image {
            Some(image) => {
                let x = index % current_width;
                let y = index / current_width;
                let position = (y >> meta_bits)
                    .checked_mul(meta_width)
                    .and_then(|row| row.checked_add(x >> meta_bits))
                    .ok_or_else(|| webp_err("VP8L meta image index overflow"))?;
                *image
                    .get(position)
                    .ok_or_else(|| webp_err("VP8L meta image index out of range"))?
            }
            None => 0,
        };
        let codes = groups
            .get(group)
            .ok_or_else(|| webp_err("VP8L Huffman group out of range"))?;
        let green_symbol = codes[0].decode(reader)?;
        let pixel: [u8; 4] = if green_symbol < 256 {
            let red = codes[1].decode(reader)? as u8;
            let blue = codes[2].decode(reader)? as u8;
            let alpha = codes[3].decode(reader)? as u8;
            [alpha, red, green_symbol as u8, blue]
        } else if green_symbol < 280 {
            let length = vp8l_lex(reader, green_symbol - 256)?;
            let distance_symbol = codes[4].decode(reader)?;
            let distance = vp8l_distance(reader, distance_symbol, current_width)?;
            if distance > index || length == 0 {
                return Err(webp_err("invalid VP8L backward reference"));
            }
            let end = index
                .checked_add(length)
                .ok_or_else(|| webp_err("VP8L backward reference overflow"))?;
            if end > pixel_count {
                return Err(webp_err("VP8L backward reference exceeds image"));
            }
            for offset in 0..length {
                let source = index - distance + offset;
                let value = pixels[source];
                pixels[index + offset] = value;
            }
            index = end;
            continue;
        } else {
            *color_cache
                .get(green_symbol - 280)
                .ok_or_else(|| webp_err("VP8L colour cache index out of range"))?
        };

        if cache_size > 0 {
            let argb = (u32::from(pixel[0]) << 24)
                | (u32::from(pixel[1]) << 16)
                | (u32::from(pixel[2]) << 8)
                | u32::from(pixel[3]);
            let hash = ((0x1e35_a7bd_u64 * u64::from(argb)) >> (32 - cache_bits)) as usize;
            if let Some(slot) = color_cache.get_mut(hash) {
                *slot = pixel;
            }
        }
        pixels[index] = pixel;
        index += 1;
    }

    // Inverse transforms are applied in the reverse of the order they were
    // read. The colour-indexing transform changes the width, so each transform
    // is paired with the resolution it was authored at.
    let mut active_width = current_width;
    for transform in transforms.iter().rev() {
        match transform {
            Vp8LTransform::Predictor {
                size_bits,
                transform_width,
                modes,
            } => {
                vp8l_predictor_inverse(
                    &mut pixels,
                    active_width,
                    height,
                    *size_bits,
                    *transform_width,
                    modes,
                )?;
            }
            Vp8LTransform::Color {
                size_bits,
                transform_width,
                elements,
            } => {
                let block = 1_usize << *size_bits;
                for y in 0..height {
                    for x in 0..active_width {
                        let position = (y >> *size_bits) * transform_width + (x >> *size_bits);
                        let element = *elements
                            .get(position)
                            .ok_or_else(|| webp_err("VP8L colour transform index out of range"))?;
                        let pixel = &mut pixels[y * active_width + x];
                        let green = i16::from(pixel[2] as i8);
                        let mut red = i16::from(pixel[1]);
                        let mut blue = i16::from(pixel[3]);
                        red += (i16::from(element[2] as i8) * green) >> 5;
                        blue += (i16::from(element[1] as i8) * green) >> 5;
                        blue += (i16::from(element[0] as i8) * i16::from(red as u8 as i8)) >> 5;
                        pixel[1] = red as u8;
                        pixel[3] = blue as u8;
                        let _ = block;
                    }
                }
            }
            Vp8LTransform::SubtractGreen => {
                for pixel in &mut pixels {
                    pixel[1] = pixel[1].wrapping_add(pixel[2]);
                    pixel[3] = pixel[3].wrapping_add(pixel[2]);
                }
            }
            Vp8LTransform::ColorIndex {
                table,
                width_bits,
                input_width,
                output_width,
            } => {
                let group = 1_usize << *width_bits;
                let index_bits = 8 / group;
                let mask = (1_u8 << index_bits) - 1;
                let mut expanded = vec![[0_u8; 4]; *output_width * height];
                for y in 0..height {
                    for packed_x in 0..*input_width {
                        let packed = pixels[y * input_width + packed_x][2];
                        for sub in 0..group {
                            let x = packed_x * group + sub;
                            if x >= *output_width {
                                break;
                            }
                            let shift = sub * index_bits;
                            let slot = usize::from((packed >> shift) & mask);
                            expanded[y * output_width + x] =
                                table.get(slot).copied().unwrap_or([0, 0, 0, 0]);
                        }
                    }
                }
                pixels = expanded;
                active_width = *output_width;
            }
        }
    }
    if active_width != width {
        return Err(webp_err("VP8L transform width does not match image"));
    }
    Ok(pixels)
}

#[allow(clippy::too_many_arguments)]
fn vp8l_predictor_inverse(
    pixels: &mut [[u8; 4]],
    width: usize,
    height: usize,
    size_bits: u32,
    transform_width: usize,
    modes: &[u8],
) -> Result<(), DecodeError> {
    let avg2 = |a: u8, b: u8| -> u8 { (u16::from(a) + u16::from(b)) as u8 / 2 };
    let clamp = |value: i32| -> u8 { value.clamp(0, 255) as u8 };
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let prediction: [u8; 4] = if index == 0 {
                [255, 0, 0, 0]
            } else if y == 0 {
                pixels[index - 1]
            } else if x == 0 {
                pixels[index - width]
            } else {
                let position = (y >> size_bits) * transform_width + (x >> size_bits);
                let mode = *modes
                    .get(position)
                    .ok_or_else(|| webp_err("VP8L predictor index out of range"))?;
                let left = pixels[index - 1];
                let top = pixels[index - width];
                let top_left = pixels[index - width - 1];
                let top_right = if x + 1 < width {
                    pixels[index - width + 1]
                } else {
                    pixels[y * width]
                };
                let mut result = [0_u8; 4];
                match mode {
                    0 => result = [255, 0, 0, 0],
                    1 => result = left,
                    2 => result = top,
                    3 => result = top_right,
                    4 => result = top_left,
                    5 => {
                        for channel in 0..4 {
                            result[channel] =
                                avg2(avg2(left[channel], top_right[channel]), top[channel]);
                        }
                    }
                    6 => {
                        for channel in 0..4 {
                            result[channel] = avg2(left[channel], top_left[channel]);
                        }
                    }
                    7 => {
                        for channel in 0..4 {
                            result[channel] = avg2(left[channel], top[channel]);
                        }
                    }
                    8 => {
                        for channel in 0..4 {
                            result[channel] = avg2(top_left[channel], top[channel]);
                        }
                    }
                    9 => {
                        for channel in 0..4 {
                            result[channel] = avg2(top[channel], top_right[channel]);
                        }
                    }
                    10 => {
                        for channel in 0..4 {
                            result[channel] = avg2(
                                avg2(left[channel], top_left[channel]),
                                avg2(top[channel], top_right[channel]),
                            );
                        }
                    }
                    11 => {
                        let mut estimate = [0_i32; 4];
                        for channel in 0..4 {
                            estimate[channel] = i32::from(left[channel]) + i32::from(top[channel])
                                - i32::from(top_left[channel]);
                        }
                        let distance_left: i32 = (0..4)
                            .map(|channel| (estimate[channel] - i32::from(left[channel])).abs())
                            .sum();
                        let distance_top: i32 = (0..4)
                            .map(|channel| (estimate[channel] - i32::from(top[channel])).abs())
                            .sum();
                        result = if distance_left < distance_top {
                            left
                        } else {
                            top
                        };
                    }
                    12 => {
                        for channel in 0..4 {
                            result[channel] = clamp(
                                i32::from(left[channel]) + i32::from(top[channel])
                                    - i32::from(top_left[channel]),
                            );
                        }
                    }
                    13 => {
                        for channel in 0..4 {
                            let average =
                                (u16::from(left[channel]) + u16::from(top[channel])) as u8 / 2;
                            result[channel] = clamp(
                                i32::from(average)
                                    + (i32::from(average) - i32::from(top_left[channel])) / 2,
                            );
                        }
                    }
                    _ => return Err(webp_err("invalid VP8L predictor mode")),
                }
                result
            };
            for channel in 0..4 {
                pixels[index][channel] = pixels[index][channel].wrapping_add(prediction[channel]);
            }
        }
    }
    Ok(())
}

/// Decodes a complete VP8L chunk (including its 5-byte header) into RGBA8.
fn decode_vp8l(data: &[u8]) -> Result<(u32, u32, Vec<u8>), DecodeError> {
    if data.len() < 5 || data[0] != 0x2f {
        return Err(webp_err("invalid VP8L signature"));
    }
    let mut reader = Vp8LReader::new(&data[1..]);
    let width = reader.read_bits(14)? + 1;
    let height = reader.read_bits(14)? + 1;
    let _alpha_is_used = reader.read_bit()?;
    if reader.read_bits(3)? != 0 {
        return Err(webp_unsupported("VP8L versions other than 0"));
    }
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(webp_err(format!(
            "VP8L dimensions {width}x{height} are invalid or exceed the {MAX_DIMENSION} pixel limit"
        )));
    }
    check_pixel_count(width, height, "VP8L")?;

    let argb = vp8l_decode_stream(&mut reader, width as usize, height as usize, true)?;
    let mut pixels = Vec::with_capacity(argb.len() * 4);
    for color in argb {
        pixels.extend_from_slice(&[color[1], color[2], color[3], color[0]]);
    }
    Ok((width, height, pixels))
}

const VP8_COEFFSPROBA0: [[[[u8; 11]; 3]; 8]; 4] = [
    [
        [
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [253, 136, 254, 255, 228, 219, 128, 128, 128, 128, 128],
            [189, 129, 242, 255, 227, 213, 255, 219, 128, 128, 128],
            [106, 126, 227, 252, 214, 209, 255, 255, 128, 128, 128],
        ],
        [
            [1, 98, 248, 255, 236, 226, 255, 255, 128, 128, 128],
            [181, 133, 238, 254, 221, 234, 255, 154, 128, 128, 128],
            [78, 134, 202, 247, 198, 180, 255, 219, 128, 128, 128],
        ],
        [
            [1, 185, 249, 255, 243, 255, 128, 128, 128, 128, 128],
            [184, 150, 247, 255, 236, 224, 128, 128, 128, 128, 128],
            [77, 110, 216, 255, 236, 230, 128, 128, 128, 128, 128],
        ],
        [
            [1, 101, 251, 255, 241, 255, 128, 128, 128, 128, 128],
            [170, 139, 241, 252, 236, 209, 255, 255, 128, 128, 128],
            [37, 116, 196, 243, 228, 255, 255, 255, 128, 128, 128],
        ],
        [
            [1, 204, 254, 255, 245, 255, 128, 128, 128, 128, 128],
            [207, 160, 250, 255, 238, 128, 128, 128, 128, 128, 128],
            [102, 103, 231, 255, 211, 171, 128, 128, 128, 128, 128],
        ],
        [
            [1, 152, 252, 255, 240, 255, 128, 128, 128, 128, 128],
            [177, 135, 243, 255, 234, 225, 128, 128, 128, 128, 128],
            [80, 129, 211, 255, 194, 224, 128, 128, 128, 128, 128],
        ],
        [
            [1, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [246, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [255, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
    ],
    [
        [
            [198, 35, 237, 223, 193, 187, 162, 160, 145, 155, 62],
            [131, 45, 198, 221, 172, 176, 220, 157, 252, 221, 1],
            [68, 47, 146, 208, 149, 167, 221, 162, 255, 223, 128],
        ],
        [
            [1, 149, 241, 255, 221, 224, 255, 255, 128, 128, 128],
            [184, 141, 234, 253, 222, 220, 255, 199, 128, 128, 128],
            [81, 99, 181, 242, 176, 190, 249, 202, 255, 255, 128],
        ],
        [
            [1, 129, 232, 253, 214, 197, 242, 196, 255, 255, 128],
            [99, 121, 210, 250, 201, 198, 255, 202, 128, 128, 128],
            [23, 91, 163, 242, 170, 187, 247, 210, 255, 255, 128],
        ],
        [
            [1, 200, 246, 255, 234, 255, 128, 128, 128, 128, 128],
            [109, 178, 241, 255, 231, 245, 255, 255, 128, 128, 128],
            [44, 130, 201, 253, 205, 192, 255, 255, 128, 128, 128],
        ],
        [
            [1, 132, 239, 251, 219, 209, 255, 165, 128, 128, 128],
            [94, 136, 225, 251, 218, 190, 255, 255, 128, 128, 128],
            [22, 100, 174, 245, 186, 161, 255, 199, 128, 128, 128],
        ],
        [
            [1, 182, 249, 255, 232, 235, 128, 128, 128, 128, 128],
            [124, 143, 241, 255, 227, 234, 128, 128, 128, 128, 128],
            [35, 77, 181, 251, 193, 211, 255, 205, 128, 128, 128],
        ],
        [
            [1, 157, 247, 255, 236, 231, 255, 255, 128, 128, 128],
            [121, 141, 235, 255, 225, 227, 255, 255, 128, 128, 128],
            [45, 99, 188, 251, 195, 217, 255, 224, 128, 128, 128],
        ],
        [
            [1, 1, 251, 255, 213, 255, 128, 128, 128, 128, 128],
            [203, 1, 248, 255, 255, 128, 128, 128, 128, 128, 128],
            [137, 1, 177, 255, 224, 255, 128, 128, 128, 128, 128],
        ],
    ],
    [
        [
            [253, 9, 248, 251, 207, 208, 255, 192, 128, 128, 128],
            [175, 13, 224, 243, 193, 185, 249, 198, 255, 255, 128],
            [73, 17, 171, 221, 161, 179, 236, 167, 255, 234, 128],
        ],
        [
            [1, 95, 247, 253, 212, 183, 255, 255, 128, 128, 128],
            [239, 90, 244, 250, 211, 209, 255, 255, 128, 128, 128],
            [155, 77, 195, 248, 188, 195, 255, 255, 128, 128, 128],
        ],
        [
            [1, 24, 239, 251, 218, 219, 255, 205, 128, 128, 128],
            [201, 51, 219, 255, 196, 186, 128, 128, 128, 128, 128],
            [69, 46, 190, 239, 201, 218, 255, 228, 128, 128, 128],
        ],
        [
            [1, 191, 251, 255, 255, 128, 128, 128, 128, 128, 128],
            [223, 165, 249, 255, 213, 255, 128, 128, 128, 128, 128],
            [141, 124, 248, 255, 255, 128, 128, 128, 128, 128, 128],
        ],
        [
            [1, 16, 248, 255, 255, 128, 128, 128, 128, 128, 128],
            [190, 36, 230, 255, 236, 255, 128, 128, 128, 128, 128],
            [149, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [1, 226, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [247, 192, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [240, 128, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [1, 134, 252, 255, 255, 128, 128, 128, 128, 128, 128],
            [213, 62, 250, 255, 255, 128, 128, 128, 128, 128, 128],
            [55, 93, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
    ],
    [
        [
            [202, 24, 213, 235, 186, 191, 220, 160, 240, 175, 255],
            [126, 38, 182, 232, 169, 184, 228, 174, 255, 187, 128],
            [61, 46, 138, 219, 151, 178, 240, 170, 255, 216, 128],
        ],
        [
            [1, 112, 230, 250, 199, 191, 247, 159, 255, 255, 128],
            [166, 109, 228, 252, 211, 215, 255, 174, 128, 128, 128],
            [39, 77, 162, 232, 172, 180, 245, 178, 255, 255, 128],
        ],
        [
            [1, 52, 220, 246, 198, 199, 249, 220, 255, 255, 128],
            [124, 74, 191, 243, 183, 193, 250, 221, 255, 255, 128],
            [24, 71, 130, 219, 154, 170, 243, 182, 255, 255, 128],
        ],
        [
            [1, 182, 225, 249, 219, 240, 255, 224, 128, 128, 128],
            [149, 150, 226, 252, 216, 205, 255, 171, 128, 128, 128],
            [28, 108, 170, 242, 183, 194, 254, 223, 255, 255, 128],
        ],
        [
            [1, 81, 230, 252, 204, 203, 255, 192, 128, 128, 128],
            [123, 102, 209, 247, 188, 196, 255, 233, 128, 128, 128],
            [20, 95, 153, 243, 164, 173, 255, 203, 128, 128, 128],
        ],
        [
            [1, 222, 248, 255, 216, 213, 128, 128, 128, 128, 128],
            [168, 175, 246, 252, 235, 205, 255, 255, 128, 128, 128],
            [47, 116, 215, 255, 211, 212, 255, 255, 128, 128, 128],
        ],
        [
            [1, 121, 236, 253, 212, 214, 255, 255, 128, 128, 128],
            [141, 84, 213, 252, 201, 202, 255, 219, 128, 128, 128],
            [42, 80, 160, 240, 162, 185, 255, 205, 128, 128, 128],
        ],
        [
            [1, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [244, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [238, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
    ],
];
const VP8_COEFFSUPDATEPROBA: [[[[u8; 11]; 3]; 8]; 4] = [
    [
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [176, 246, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [223, 241, 252, 255, 255, 255, 255, 255, 255, 255, 255],
            [249, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 244, 252, 255, 255, 255, 255, 255, 255, 255, 255],
            [234, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 246, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [239, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 248, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [251, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [251, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 253, 255, 254, 255, 255, 255, 255, 255, 255],
            [250, 255, 254, 255, 254, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
    [
        [
            [217, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [225, 252, 241, 253, 255, 255, 254, 255, 255, 255, 255],
            [234, 250, 241, 250, 253, 255, 253, 254, 255, 255, 255],
        ],
        [
            [255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [223, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [238, 253, 254, 254, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 248, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [249, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [247, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [252, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [250, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
    [
        [
            [186, 251, 250, 255, 255, 255, 255, 255, 255, 255, 255],
            [234, 251, 244, 254, 255, 255, 255, 255, 255, 255, 255],
            [251, 251, 243, 253, 254, 255, 254, 255, 255, 255, 255],
        ],
        [
            [255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [236, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [251, 253, 253, 254, 254, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
    [
        [
            [248, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [250, 254, 252, 254, 255, 255, 255, 255, 255, 255, 255],
            [248, 254, 249, 253, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [246, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [252, 254, 251, 254, 254, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 252, 255, 255, 255, 255, 255, 255, 255, 255],
            [248, 254, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 255, 254, 254, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 251, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [245, 251, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 251, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [252, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 252, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [249, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [250, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
];
const VP8_KBMODESPROBA: [[[u8; 9]; 10]; 10] = [
    [
        [231, 120, 48, 89, 115, 113, 120, 152, 112],
        [152, 179, 64, 126, 170, 118, 46, 70, 95],
        [175, 69, 143, 80, 85, 82, 72, 155, 103],
        [56, 58, 10, 171, 218, 189, 17, 13, 152],
        [114, 26, 17, 163, 44, 195, 21, 10, 173],
        [121, 24, 80, 195, 26, 62, 44, 64, 85],
        [144, 71, 10, 38, 171, 213, 144, 34, 26],
        [170, 46, 55, 19, 136, 160, 33, 206, 71],
        [63, 20, 8, 114, 114, 208, 12, 9, 226],
        [81, 40, 11, 96, 182, 84, 29, 16, 36],
    ],
    [
        [134, 183, 89, 137, 98, 101, 106, 165, 148],
        [72, 187, 100, 130, 157, 111, 32, 75, 80],
        [66, 102, 167, 99, 74, 62, 40, 234, 128],
        [41, 53, 9, 178, 241, 141, 26, 8, 107],
        [74, 43, 26, 146, 73, 166, 49, 23, 157],
        [65, 38, 105, 160, 51, 52, 31, 115, 128],
        [104, 79, 12, 27, 217, 255, 87, 17, 7],
        [87, 68, 71, 44, 114, 51, 15, 186, 23],
        [47, 41, 14, 110, 182, 183, 21, 17, 194],
        [66, 45, 25, 102, 197, 189, 23, 18, 22],
    ],
    [
        [88, 88, 147, 150, 42, 46, 45, 196, 205],
        [43, 97, 183, 117, 85, 38, 35, 179, 61],
        [39, 53, 200, 87, 26, 21, 43, 232, 171],
        [56, 34, 51, 104, 114, 102, 29, 93, 77],
        [39, 28, 85, 171, 58, 165, 90, 98, 64],
        [34, 22, 116, 206, 23, 34, 43, 166, 73],
        [107, 54, 32, 26, 51, 1, 81, 43, 31],
        [68, 25, 106, 22, 64, 171, 36, 225, 114],
        [34, 19, 21, 102, 132, 188, 16, 76, 124],
        [62, 18, 78, 95, 85, 57, 50, 48, 51],
    ],
    [
        [193, 101, 35, 159, 215, 111, 89, 46, 111],
        [60, 148, 31, 172, 219, 228, 21, 18, 111],
        [112, 113, 77, 85, 179, 255, 38, 120, 114],
        [40, 42, 1, 196, 245, 209, 10, 25, 109],
        [88, 43, 29, 140, 166, 213, 37, 43, 154],
        [61, 63, 30, 155, 67, 45, 68, 1, 209],
        [100, 80, 8, 43, 154, 1, 51, 26, 71],
        [142, 78, 78, 16, 255, 128, 34, 197, 171],
        [41, 40, 5, 102, 211, 183, 4, 1, 221],
        [51, 50, 17, 168, 209, 192, 23, 25, 82],
    ],
    [
        [138, 31, 36, 171, 27, 166, 38, 44, 229],
        [67, 87, 58, 169, 82, 115, 26, 59, 179],
        [63, 59, 90, 180, 59, 166, 93, 73, 154],
        [40, 40, 21, 116, 143, 209, 34, 39, 175],
        [47, 15, 16, 183, 34, 223, 49, 45, 183],
        [46, 17, 33, 183, 6, 98, 15, 32, 183],
        [57, 46, 22, 24, 128, 1, 54, 17, 37],
        [65, 32, 73, 115, 28, 128, 23, 128, 205],
        [40, 3, 9, 115, 51, 192, 18, 6, 223],
        [87, 37, 9, 115, 59, 77, 64, 21, 47],
    ],
    [
        [104, 55, 44, 218, 9, 54, 53, 130, 226],
        [64, 90, 70, 205, 40, 41, 23, 26, 57],
        [54, 57, 112, 184, 5, 41, 38, 166, 213],
        [30, 34, 26, 133, 152, 116, 10, 32, 134],
        [39, 19, 53, 221, 26, 114, 32, 73, 255],
        [31, 9, 65, 234, 2, 15, 1, 118, 73],
        [75, 32, 12, 51, 192, 255, 160, 43, 51],
        [88, 31, 35, 67, 102, 85, 55, 186, 85],
        [56, 21, 23, 111, 59, 205, 45, 37, 192],
        [55, 38, 70, 124, 73, 102, 1, 34, 98],
    ],
    [
        [125, 98, 42, 88, 104, 85, 117, 175, 82],
        [95, 84, 53, 89, 128, 100, 113, 101, 45],
        [75, 79, 123, 47, 51, 128, 81, 171, 1],
        [57, 17, 5, 71, 102, 57, 53, 41, 49],
        [38, 33, 13, 121, 57, 73, 26, 1, 85],
        [41, 10, 67, 138, 77, 110, 90, 47, 114],
        [115, 21, 2, 10, 102, 255, 166, 23, 6],
        [101, 29, 16, 10, 85, 128, 101, 196, 26],
        [57, 18, 10, 102, 102, 213, 34, 20, 43],
        [117, 20, 15, 36, 163, 128, 68, 1, 26],
    ],
    [
        [102, 61, 71, 37, 34, 53, 31, 243, 192],
        [69, 60, 71, 38, 73, 119, 28, 222, 37],
        [68, 45, 128, 34, 1, 47, 11, 245, 171],
        [62, 17, 19, 70, 146, 85, 55, 62, 70],
        [37, 43, 37, 154, 100, 163, 85, 160, 1],
        [63, 9, 92, 136, 28, 64, 32, 201, 85],
        [75, 15, 9, 9, 64, 255, 184, 119, 16],
        [86, 6, 28, 5, 64, 255, 25, 248, 1],
        [56, 8, 17, 132, 137, 255, 55, 116, 128],
        [58, 15, 20, 82, 135, 57, 26, 121, 40],
    ],
    [
        [164, 50, 31, 137, 154, 133, 25, 35, 218],
        [51, 103, 44, 131, 131, 123, 31, 6, 158],
        [86, 40, 64, 135, 148, 224, 45, 183, 128],
        [22, 26, 17, 131, 240, 154, 14, 1, 209],
        [45, 16, 21, 91, 64, 222, 7, 1, 197],
        [56, 21, 39, 155, 60, 138, 23, 102, 213],
        [83, 12, 13, 54, 192, 255, 68, 47, 28],
        [85, 26, 85, 85, 128, 128, 32, 146, 171],
        [18, 11, 7, 63, 144, 171, 4, 4, 246],
        [35, 27, 10, 146, 174, 171, 12, 26, 128],
    ],
    [
        [190, 80, 35, 99, 180, 80, 126, 54, 45],
        [85, 126, 47, 87, 176, 51, 41, 20, 32],
        [101, 75, 128, 139, 118, 146, 116, 128, 85],
        [56, 41, 15, 176, 236, 85, 37, 9, 62],
        [71, 30, 17, 119, 118, 255, 17, 18, 138],
        [101, 38, 60, 138, 55, 70, 43, 26, 142],
        [146, 36, 19, 30, 171, 255, 97, 27, 20],
        [138, 45, 61, 62, 219, 1, 81, 188, 64],
        [32, 41, 20, 117, 151, 142, 20, 21, 163],
        [112, 19, 12, 61, 195, 128, 48, 4, 24],
    ],
];

// -------------------------------------------------------------------------
// VP8 (single key frame)
// -------------------------------------------------------------------------

/// MSB-first boolean decoder used by all VP8 data partitions.
struct Vp8BoolReader<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    value: u32,
    bit_count: u32,
}

impl<'a> Vp8BoolReader<'a> {
    fn new(data: &'a [u8]) -> Result<Self, DecodeError> {
        if data.len() < 2 {
            return Err(webp_err("truncated VP8 data partition"));
        }
        Ok(Self {
            data,
            pos: 2,
            range: 255,
            value: (u32::from(data[0]) << 8) | u32::from(data[1]),
            bit_count: 0,
        })
    }

    /// Reads one bool whose probability of being zero is `prob / 256`.
    fn read_bool(&mut self, prob: u8) -> Result<u32, DecodeError> {
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        let split8 = split << 8;
        let result = if self.value >= split8 {
            self.value -= split8;
            self.range -= split;
            1
        } else {
            self.range = split;
            0
        };
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.bit_count = 0;
                // libwebp pads exhausted partitions with zero bits instead
                // of failing; malformed streams simply decode as defaults.
                let byte = self.data.get(self.pos).copied().unwrap_or(0);
                self.pos += 1;
                self.value |= u32::from(byte);
            }
        }
        Ok(result)
    }

    /// Reads an unsigned `count`-bit literal (MSB first, probability 128).
    fn read_literal(&mut self, count: u32) -> Result<u32, DecodeError> {
        let mut value = 0_u32;
        for _ in 0..count {
            value = (value << 1) | self.read_bool(128)?;
        }
        Ok(value)
    }

    /// Reads VP8's signed literal encoding.
    fn read_signed(&mut self, count: u32) -> Result<i32, DecodeError> {
        if count == 0 {
            return Ok(0);
        }
        let mut value = if self.read_bool(128)? == 1 { -1_i32 } else { 0 };
        let mut remaining = count;
        while remaining > 1 {
            value = (value << 1) + self.read_bool(128)? as i32;
            remaining -= 1;
        }
        Ok(value)
    }
}

/// Dequantization matrix for one plane type. Index 0 is DC, 1 is AC.
#[derive(Clone, Copy)]
struct Vp8Quant {
    dc: i32,
    ac: i32,
}

/// Filter parameters for one frame/segment/mode combination.
#[derive(Clone, Copy)]
struct Vp8FilterStrength {
    level: u8,
    interior: u8,
    hev: u8,
}

impl Vp8FilterStrength {
    fn none() -> Self {
        Self {
            level: 0,
            interior: 0,
            hev: 0,
        }
    }
}

/// Everything the macroblock decoder needs from a key frame header.
struct Vp8Frame<'a> {
    width: u32,
    height: u32,
    mb_width: usize,
    mb_height: usize,
    scale_h: u32,
    scale_v: u32,
    quant: Vec<Vp8Quant>,
    segment_enabled: bool,
    segment_probs: [u8; 3],
    filters: Vec<[Vp8FilterStrength; 2]>,
    simple_filter: bool,
    use_skip_proba: bool,
    skip_p: u8,
    token_probs: Box<[[[[u8; 11]; 3]; 8]; 4]>,
    partitions: Vec<&'a [u8]>,
}

// Index -> coefficient band (RFC 6386 section 13.3).
const VP8_BANDS: [usize; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];

const VP8_ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

const VP8_DC_Q: [i32; 128] = [
    4, 5, 6, 7, 8, 9, 10, 10, 11, 12, 13, 14, 15, 16, 17, 17, 18, 19, 20, 20, 21, 21, 22, 22, 23,
    23, 24, 25, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 37, 38, 39, 40, 41, 42, 43, 44,
    45, 46, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
    68, 69, 70, 71, 72, 73, 74, 75, 76, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 91,
    93, 95, 96, 98, 100, 101, 102, 104, 106, 108, 110, 112, 114, 116, 118, 122, 124, 126, 128, 130,
    132, 134, 136, 138, 140, 143, 145, 148, 151, 154, 157,
];

const VP8_AC_Q: [i32; 128] = [
    4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52,
    53, 54, 55, 56, 57, 58, 60, 62, 64, 66, 68, 70, 72, 74, 76, 78, 80, 82, 84, 86, 88, 90, 92, 94,
    96, 98, 100, 102, 104, 106, 108, 110, 112, 114, 116, 119, 122, 125, 128, 131, 134, 137, 140,
    143, 146, 149, 152, 155, 158, 161, 164, 167, 170, 173, 177, 181, 185, 189, 193, 197, 201, 205,
    209, 213, 217, 221, 225, 229, 234, 239, 245, 249, 254, 259, 264, 269, 274, 279, 284,
];

// Probability tables for the "category" tokens dct_cat3..dct_cat6.
const VP8_CAT_PROBS: [&[u8]; 4] = [
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];

fn vp8_clamp(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn vp8_saturate_i8(value: i32) -> i32 {
    value.clamp(-128, 127)
}

/// Parses the uncompressed frame tag, key-frame header and token partitions.
fn vp8_parse_frame(data: &[u8]) -> Result<(Vp8Frame<'_>, Vp8BoolReader<'_>), DecodeError> {
    if data.len() < 10 {
        return Err(webp_err("truncated VP8 frame tag"));
    }
    let tag = webp_u24le(data, 0)?;
    if tag & 1 != 0 {
        return Err(webp_unsupported("VP8 interframes"));
    }
    let version = (tag >> 1) & 7;
    if version > 3 {
        return Err(webp_err("invalid VP8 version"));
    }
    if tag & 0x10 == 0 {
        return Err(webp_unsupported("VP8 frames with show_frame = 0"));
    }
    let first_partition_size = (tag >> 5) as usize;
    if data[3..6] != [0x9d, 0x01, 0x2a] {
        return Err(webp_err("invalid VP8 key frame start code"));
    }
    let width_word = webp_u16le(data, 6)?;
    let height_word = webp_u16le(data, 8)?;
    let width = u32::from(width_word & 0x3fff);
    let height = u32::from(height_word & 0x3fff);
    let scale_h = u32::from(width_word >> 14);
    let scale_v = u32::from(height_word >> 14);
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(webp_err(format!(
            "VP8 dimensions {width}x{height} are invalid or exceed the {MAX_DIMENSION} pixel limit"
        )));
    }
    check_pixel_count(width, height, "VP8")?;
    let first_end = 10_usize
        .checked_add(first_partition_size)
        .ok_or_else(|| webp_err("VP8 partition size overflow"))?;
    if first_end > data.len() || first_partition_size == 0 {
        return Err(webp_err("truncated VP8 first partition"));
    }
    let mut reader = Vp8BoolReader::new(&data[10..first_end])?;

    // 9.2: colour space and clamp type (hints only).
    let _color_space = reader.read_bool(128)?;
    let _clamp_type = reader.read_bool(128)?;

    // 9.3: segmentation.
    let segment_enabled = reader.read_bool(128)? != 0;
    let mut update_map = false;
    let mut segment_quant = [0_i32; 4];
    let mut segment_filter = [0_i32; 4];
    let mut absolute_delta = false;
    let mut segment_probs = [255_u8; 3];
    if segment_enabled {
        update_map = reader.read_bool(128)? != 0;
        let update_features = reader.read_bool(128)? != 0;
        if update_features {
            absolute_delta = reader.read_bool(128)? != 0;
            for slot in &mut segment_quant {
                if reader.read_bool(128)? != 0 {
                    let magnitude = reader.read_literal(7)? as i32;
                    let sign = reader.read_bool(128)?;
                    *slot = if sign == 1 { -magnitude } else { magnitude };
                }
            }
            for slot in &mut segment_filter {
                if reader.read_bool(128)? != 0 {
                    let magnitude = reader.read_literal(6)? as i32;
                    let sign = reader.read_bool(128)?;
                    *slot = if sign == 1 { -magnitude } else { magnitude };
                }
            }
        }
        if update_map {
            for slot in &mut segment_probs {
                if reader.read_bool(128)? != 0 {
                    *slot = reader.read_literal(8)? as u8;
                }
            }
        }
    }

    // 9.4: loop filter.
    let simple_filter = reader.read_bool(128)? != 0;
    let filter_level = reader.read_literal(6)? as i32;
    let sharpness = reader.read_literal(3)?;
    let lf_adjust_enabled = reader.read_bool(128)? != 0;
    let mut ref_lf_delta = [0_i32; 4];
    let mut mode_lf_delta = [0_i32; 4];
    if lf_adjust_enabled && reader.read_bool(128)? != 0 {
        for slot in &mut ref_lf_delta {
            if reader.read_bool(128)? != 0 {
                *slot = reader.read_signed(6)?;
            }
        }
        for slot in &mut mode_lf_delta {
            if reader.read_bool(128)? != 0 {
                *slot = reader.read_signed(6)?;
            }
        }
    }

    // 9.5: how many coefficient partitions (the sizes live after partition 0).
    let log2_partitions = reader.read_literal(2)?;
    let num_partitions = 1_usize << log2_partitions;

    // 9.6: quantizer indices.
    let base_q = reader.read_literal(7)? as i32;
    let mut quant_deltas = [0_i32; 5];
    for slot in &mut quant_deltas {
        if reader.read_bool(128)? != 0 {
            *slot = reader.read_signed(4)?;
        }
    }

    // Key frames ignore the entropy-refresh flag.
    let _refresh_entropy = reader.read_bool(128)?;

    // 9.9: token probability updates (defaults restored on every key frame).
    let mut token_probs = Box::new(VP8_COEFFSPROBA0);
    for t in 0..4 {
        for b in 0..8 {
            for c in 0..3 {
                for p in 0..11 {
                    if reader.read_bool(VP8_COEFFSUPDATEPROBA[t][b][c][p])? != 0 {
                        token_probs[t][b][c][p] = reader.read_literal(8)? as u8;
                    }
                }
            }
        }
    }

    let use_skip_proba = reader.read_bool(128)? != 0;
    let skip_p = if use_skip_proba {
        reader.read_literal(8)? as u8
    } else {
        0
    };

    // Partition sizes are three raw little-endian bytes immediately after the
    // first partition, followed by the partition payloads themselves.
    let size_table = &data[first_end..];
    if size_table.len() < (num_partitions - 1) * 3 {
        return Err(webp_err("truncated VP8 partition size table"));
    }
    let mut sizes = Vec::with_capacity(num_partitions);
    for part in 0..num_partitions - 1 {
        sizes.push(webp_u24le(size_table, part * 3)? as usize);
    }
    let mut cursor = first_end + (num_partitions - 1) * 3;
    let mut partitions = Vec::with_capacity(num_partitions);
    for (part, size) in sizes.iter().enumerate() {
        let end = cursor
            .checked_add(*size)
            .ok_or_else(|| webp_err("VP8 partition size overflow"))?;
        if end > data.len() {
            return Err(webp_err("truncated VP8 data partition"));
        }
        // libwebp clamps a partition that runs past the end of the buffer.
        if part + 1 == num_partitions {
            partitions.push(&data[cursor..]);
        } else {
            partitions.push(&data[cursor..end]);
        }
        cursor = end;
    }
    if num_partitions == 1 {
        partitions.push(&data[first_end..]);
    }

    // Precompute per-segment quantizers and filter strengths.
    let mut quant = Vec::with_capacity(4);
    let mut filters = Vec::with_capacity(4);
    for segment in 0..4 {
        let q = if segment_enabled {
            if absolute_delta {
                segment_quant[segment]
            } else {
                base_q + segment_quant[segment]
            }
        } else {
            base_q
        }
        .clamp(0, 127) as usize;
        let y_dc = VP8_DC_Q[(q as i32 + quant_deltas[0]).clamp(0, 127) as usize];
        let y2_dc = 2 * VP8_DC_Q[(q as i32 + quant_deltas[1]).clamp(0, 127) as usize];
        let y2_ac = (VP8_AC_Q[(q as i32 + quant_deltas[2]).clamp(0, 127) as usize] * 101581) >> 16;
        let uv_dc = VP8_DC_Q[(q as i32 + quant_deltas[3]).clamp(0, 117) as usize];
        let uv_ac = VP8_AC_Q[(q as i32 + quant_deltas[4]).clamp(0, 127) as usize];
        quant.push(Vp8Quant {
            dc: y_dc,
            ac: VP8_AC_Q[q],
        });
        quant.push(Vp8Quant {
            dc: y2_dc,
            ac: y2_ac.max(8),
        });
        quant.push(Vp8Quant {
            dc: uv_dc,
            ac: uv_ac,
        });

        let segment_level = if segment_enabled {
            if absolute_delta {
                segment_filter[segment]
            } else {
                filter_level + segment_filter[segment]
            }
        } else {
            filter_level
        };
        let mut per_mode = [Vp8FilterStrength::none(); 2];
        for (index, strength) in per_mode.iter_mut().enumerate() {
            let mut level = segment_level;
            if lf_adjust_enabled {
                level += ref_lf_delta[0];
                if index == 1 {
                    level += mode_lf_delta[0];
                }
            }
            level = level.clamp(0, 63);
            if level > 0 {
                let mut interior = level;
                if sharpness > 0 {
                    interior >>= if sharpness > 4 { 2 } else { 1 };
                    interior = interior.min(9_i32 - sharpness as i32);
                }
                interior = interior.max(1);
                *strength = Vp8FilterStrength {
                    level: level as u8,
                    interior: interior as u8,
                    hev: if level >= 40 {
                        2
                    } else if level >= 15 {
                        1
                    } else {
                        0
                    },
                };
            }
        }
        filters.push(per_mode);
    }

    let mb_width = (width as usize).div_ceil(16);
    let mb_height = (height as usize).div_ceil(16);

    Ok((
        Vp8Frame {
            width,
            height,
            mb_width,
            mb_height,
            scale_h,
            scale_v,
            quant,
            segment_enabled: update_map,
            segment_probs,
            filters,
            simple_filter,
            use_skip_proba,
            skip_p,
            token_probs,
            partitions,
        },
        reader,
    ))
}

/// Reads the highest-value token in a VP8 coefficient block and returns its
/// magnitude (without sign).
fn vp8_large_value(reader: &mut Vp8BoolReader<'_>, probs: &[u8; 11]) -> Result<i32, DecodeError> {
    if reader.read_bool(probs[3])? == 0 {
        if reader.read_bool(probs[4])? == 0 {
            Ok(2)
        } else {
            Ok(3 + reader.read_bool(probs[5])? as i32)
        }
    } else if reader.read_bool(probs[6])? == 0 {
        if reader.read_bool(probs[7])? == 0 {
            Ok(5 + reader.read_bool(159)? as i32)
        } else {
            Ok(7 + 2 * reader.read_bool(165)? as i32 + reader.read_bool(145)? as i32)
        }
    } else {
        let bit1 = reader.read_bool(probs[8])? as usize;
        let bit0 = reader.read_bool(probs[9 + bit1])? as usize;
        let category = 2 * bit1 + bit0;
        let table = VP8_CAT_PROBS[category];
        let mut value = 0_i32;
        for &prob in table {
            value = value * 2 + reader.read_bool(prob)? as i32;
        }
        Ok(value + 3 + (8 << category))
    }
}

/// Decodes one 4x4 block of quantized, dequantized coefficients. Returns the
/// number of coefficients up to and including the last non-zero one, and fills
/// `output` in natural (row-major) order.
#[allow(clippy::too_many_arguments)]
fn vp8_read_coeffs(
    reader: &mut Vp8BoolReader<'_>,
    probs: &[[[[u8; 11]; 3]; 8]; 4],
    plane_type: usize,
    dequant: Vp8Quant,
    context: usize,
    first: usize,
    output: &mut [i32; 16],
) -> Result<usize, DecodeError> {
    output.fill(0);
    let mut index = first;
    let mut context = context;
    while index < 16 {
        let mut table = &probs[plane_type][VP8_BANDS[index]][context];
        if reader.read_bool(table[0])? == 0 {
            return Ok(index);
        }
        // Consume a run of zero coefficients; after the first zero the band's
        // context is always zero.
        loop {
            if reader.read_bool(table[1])? != 0 {
                break;
            }
            index += 1;
            if index == 16 {
                return Ok(16);
            }
            table = &probs[plane_type][VP8_BANDS[index]][0];
        }
        let magnitude = if reader.read_bool(table[2])? == 0 {
            1
        } else {
            vp8_large_value(reader, table)?
        };
        let sign = reader.read_bool(128)?;
        let quant = if index == 0 { dequant.dc } else { dequant.ac };
        let value = magnitude * quant;
        output[VP8_ZIGZAG[index]] = if sign == 1 { -value } else { value };
        context = if magnitude == 1 { 1 } else { 2 };
        index += 1;
    }
    Ok(16)
}

/// Inverse 4x4 Walsh-Hadamard transform of the Y2 block.
fn vp8_inverse_wht(input: &[i32; 16]) -> [i32; 16] {
    let mut temporary = [0_i32; 16];
    for column in 0..4 {
        let a1 = input[column] + input[column + 12];
        let b1 = input[column + 4] + input[column + 8];
        let c1 = input[column + 4] - input[column + 8];
        let d1 = input[column] - input[column + 12];
        temporary[column] = a1 + b1;
        temporary[column + 4] = c1 + d1;
        temporary[column + 8] = a1 - b1;
        temporary[column + 12] = d1 - c1;
    }
    let mut output = [0_i32; 16];
    for row in 0..4 {
        let base = row * 4;
        let a1 = temporary[base] + temporary[base + 3];
        let b1 = temporary[base + 1] + temporary[base + 2];
        let c1 = temporary[base + 1] - temporary[base + 2];
        let d1 = temporary[base] - temporary[base + 3];
        output[base] = (a1 + b1 + 3) >> 3;
        output[base + 1] = (c1 + d1 + 3) >> 3;
        output[base + 2] = (a1 - b1 + 3) >> 3;
        output[base + 3] = (d1 - c1 + 3) >> 3;
    }
    output
}

/// Fixed-point inverse 4x4 DCT from RFC 6386 section 14.4.
fn vp8_inverse_dct(input: &[i32; 16]) -> [i32; 16] {
    const COSPI8SQRT2MINUS1: i32 = 20091;
    const SINPI8SQRT2: i32 = 35468;
    let mut temporary = [0_i32; 16];
    for column in 0..4 {
        let a1 = input[column] + input[column + 8];
        let b1 = input[column] - input[column + 8];
        let temp1 = (input[column + 4] * SINPI8SQRT2) >> 16;
        let temp2 = input[column + 12] + ((input[column + 12] * COSPI8SQRT2MINUS1) >> 16);
        let c1 = temp1 - temp2;
        let temp1 = input[column + 4] + ((input[column + 4] * COSPI8SQRT2MINUS1) >> 16);
        let temp2 = (input[column + 12] * SINPI8SQRT2) >> 16;
        let d1 = temp1 + temp2;
        temporary[column] = a1 + d1;
        temporary[column + 12] = a1 - d1;
        temporary[column + 4] = b1 + c1;
        temporary[column + 8] = b1 - c1;
    }
    let mut output = [0_i32; 16];
    for row in 0..4 {
        let base = row * 4;
        let a1 = temporary[base] + temporary[base + 2];
        let b1 = temporary[base] - temporary[base + 2];
        let temp1 = (temporary[base + 1] * SINPI8SQRT2) >> 16;
        let temp2 = temporary[base + 3] + ((temporary[base + 3] * COSPI8SQRT2MINUS1) >> 16);
        let c1 = temp1 - temp2;
        let temp1 = temporary[base + 1] + ((temporary[base + 1] * COSPI8SQRT2MINUS1) >> 16);
        let temp2 = (temporary[base + 3] * SINPI8SQRT2) >> 16;
        let d1 = temp1 + temp2;
        output[base] = (a1 + d1 + 4) >> 3;
        output[base + 3] = (a1 - d1 + 4) >> 3;
        output[base + 1] = (b1 + c1 + 4) >> 3;
        output[base + 2] = (b1 - c1 + 4) >> 3;
    }
    output
}

fn vp8_above_row(plane: &[u8], stride: usize, mb_x: usize, mb_y: usize, size: usize) -> [u8; 16] {
    let mut row = [127_u8; 16];
    if mb_y > 0 {
        let start = (mb_y * size - 1) * stride + mb_x * size;
        row[..size].copy_from_slice(&plane[start..start + size]);
    }
    row
}

fn vp8_left_column(plane: &[u8], stride: usize, mb_x: usize, mb_y: usize, size: usize) -> [u8; 16] {
    let mut column = [129_u8; 16];
    if mb_x > 0 {
        for index in 0..size {
            column[index] = plane[(mb_y * size + index) * stride + mb_x * size - 1];
        }
    }
    column
}

/// Predicts one 16x16 or 8x8 intra block, including the special DC border cases.
fn vp8_intra_predict(
    plane: &[u8],
    stride: usize,
    mb_x: usize,
    mb_y: usize,
    size: usize,
    mode: usize,
) -> Result<Vec<u8>, DecodeError> {
    let above = vp8_above_row(plane, stride, mb_x, mb_y, size);
    let left = vp8_left_column(plane, stride, mb_x, mb_y, size);
    let mut block = vec![0_u8; size * size];
    match mode {
        0 => {
            let value = if mb_x == 0 && mb_y == 0 {
                128
            } else if mb_y == 0 {
                let sum: u32 = left.iter().take(size).map(|&value| u32::from(value)).sum();
                ((sum + (size as u32 / 2)) / size as u32) as u8
            } else if mb_x == 0 {
                let sum: u32 = above.iter().take(size).map(|&value| u32::from(value)).sum();
                ((sum + (size as u32 / 2)) / size as u32) as u8
            } else {
                let sum: u32 = above
                    .iter()
                    .take(size)
                    .map(|&value| u32::from(value))
                    .sum::<u32>()
                    + left
                        .iter()
                        .take(size)
                        .map(|&value| u32::from(value))
                        .sum::<u32>();
                ((sum + size as u32) / (2 * size as u32)) as u8
            };
            block.fill(value);
        }
        1 => {
            for y in 0..size {
                for x in 0..size {
                    block[y * size + x] = above[x];
                }
            }
        }
        2 => {
            for y in 0..size {
                for x in 0..size {
                    block[y * size + x] = left[y];
                }
            }
        }
        3 => {
            let top_left = if mb_x > 0 && mb_y > 0 {
                plane[(mb_y * size - 1) * stride + mb_x * size - 1]
            } else {
                127
            };
            for y in 0..size {
                for x in 0..size {
                    block[y * size + x] =
                        vp8_clamp(i32::from(left[y]) + i32::from(above[x]) - i32::from(top_left));
                }
            }
        }
        _ => return Err(webp_err("invalid VP8 intra prediction mode")),
    }
    Ok(block)
}

fn vp8_subblock_pixel(plane: &[u8], stride: usize, mb_x: usize, mb_y: usize, x: i32, y: i32) -> u8 {
    if y < 0 {
        return 127;
    }
    if x < 0 {
        return 129;
    }
    if x >= 16 {
        // Only used above the right edge of a macroblock; the reference pixel
        // is the rightmost pixel on the line above (or 127 in the top row).
        if mb_y == 0 {
            return 127;
        }
        return plane[(mb_y * 16 - 1) * stride + mb_x * 16 + 15];
    }
    let absolute = mb_x * 16 + x as usize;
    if absolute >= stride {
        return plane[y as usize * stride + stride - 1];
    }
    plane[y as usize * stride + absolute]
}

/// Predicts one 4x4 luma subblock using the ten intra-bmode formulas.
#[allow(clippy::needless_range_loop)]
fn vp8_subblock_predict(
    plane: &[u8],
    stride: usize,
    mb_x: usize,
    mb_y: usize,
    bx: usize,
    by: usize,
    mode: usize,
) -> Result<[u8; 16], DecodeError> {
    let px = (bx * 4) as i32;
    let py = (by * 4) as i32;
    let get = |x: i32, y: i32| vp8_subblock_pixel(plane, stride, mb_x, mb_y, px + x, py + y);
    let mut a = [0_u8; 8];
    for (index, slot) in a.iter_mut().enumerate() {
        *slot = get(index as i32, -1);
    }
    let mut l = [0_u8; 4];
    for (index, slot) in l.iter_mut().enumerate() {
        *slot = get(-1, index as i32);
    }
    let p = get(-1, -1);
    let e = [l[3], l[2], l[1], l[0], p, a[0], a[1], a[2], a[3]];
    let avg2 = |x: u8, y: u8| -> u8 { (u16::from(x) + u16::from(y) + 1) as u8 / 2 };
    let avg3 = |x: u8, y: u8, z: u8| -> u8 {
        (u16::from(x) + 2 * u16::from(y) + u16::from(z) + 2) as u8 / 4
    };
    let avg3_at =
        |values: &[u8], at: usize| -> u8 { avg3(values[at - 1], values[at], values[at + 1]) };
    let avg2_at = |values: &[u8], at: usize| -> u8 { avg2(values[at], values[at + 1]) };
    let mut block = [0_u8; 16];
    let set = |block: &mut [u8; 16], x: usize, y: usize, value: u8| block[y * 4 + x] = value;
    match mode {
        0 => {
            let mut sum = 4_u32;
            for index in 0..4 {
                sum += u32::from(a[index]) + u32::from(l[index]);
            }
            block.fill((sum >> 3) as u8);
        }
        1 => {
            for y in 0..4 {
                for x in 0..4 {
                    set(
                        &mut block,
                        x,
                        y,
                        vp8_clamp(i32::from(l[y]) + i32::from(a[x]) - i32::from(p)),
                    );
                }
            }
        }
        2 => {
            for x in 0..4 {
                let value = if x == 0 {
                    avg3(p, a[0], a[1])
                } else {
                    avg3_at(&a, x)
                };
                for y in 0..4 {
                    set(&mut block, x, y, value);
                }
            }
        }
        3 => {
            let mut value = avg3(l[2], l[3], l[3]);
            for y in (0..4).rev() {
                for x in 0..4 {
                    set(&mut block, x, y, value);
                }
                if y > 0 {
                    let previous = if y == 1 { p } else { l[y - 2] };
                    value = avg3(previous, l[y - 1], l[y]);
                }
            }
        }
        4 => {
            set(&mut block, 0, 0, avg3_at(&a, 1));
            set(&mut block, 1, 0, avg3_at(&a, 2));
            set(&mut block, 0, 1, avg3_at(&a, 2));
            set(&mut block, 2, 0, avg3_at(&a, 3));
            set(&mut block, 1, 1, avg3_at(&a, 3));
            set(&mut block, 0, 2, avg3_at(&a, 3));
            set(&mut block, 3, 0, avg3_at(&a, 4));
            set(&mut block, 2, 1, avg3_at(&a, 4));
            set(&mut block, 1, 2, avg3_at(&a, 4));
            set(&mut block, 0, 3, avg3_at(&a, 4));
            set(&mut block, 3, 1, avg3_at(&a, 5));
            set(&mut block, 2, 2, avg3_at(&a, 5));
            set(&mut block, 1, 3, avg3_at(&a, 5));
            set(&mut block, 3, 2, avg3_at(&a, 6));
            set(&mut block, 2, 3, avg3_at(&a, 6));
            set(&mut block, 3, 3, avg3(a[6], a[7], a[7]));
        }
        5 => {
            set(&mut block, 0, 3, avg3_at(&e, 1));
            set(&mut block, 0, 2, avg3_at(&e, 2));
            set(&mut block, 1, 3, avg3_at(&e, 2));
            set(&mut block, 0, 1, avg3_at(&e, 3));
            set(&mut block, 1, 2, avg3_at(&e, 3));
            set(&mut block, 2, 3, avg3_at(&e, 3));
            set(&mut block, 0, 0, avg3_at(&e, 4));
            set(&mut block, 1, 1, avg3_at(&e, 4));
            set(&mut block, 2, 2, avg3_at(&e, 4));
            set(&mut block, 3, 3, avg3_at(&e, 4));
            set(&mut block, 1, 0, avg3_at(&e, 5));
            set(&mut block, 2, 1, avg3_at(&e, 5));
            set(&mut block, 3, 2, avg3_at(&e, 5));
            set(&mut block, 2, 0, avg3_at(&e, 6));
            set(&mut block, 3, 1, avg3_at(&e, 6));
            set(&mut block, 3, 0, avg3_at(&e, 7));
        }
        6 => {
            set(&mut block, 0, 3, avg3_at(&e, 2));
            set(&mut block, 0, 2, avg3_at(&e, 3));
            set(&mut block, 1, 3, avg3_at(&e, 4));
            set(&mut block, 1, 0, avg3_at(&e, 4));
            set(&mut block, 0, 0, avg2_at(&e, 4));
            set(&mut block, 2, 3, avg3_at(&e, 5));
            set(&mut block, 2, 0, avg2_at(&e, 5));
            set(&mut block, 1, 1, avg3_at(&e, 5));
            set(&mut block, 3, 3, avg3_at(&e, 6));
            set(&mut block, 3, 0, avg2_at(&e, 6));
            set(&mut block, 2, 1, avg3_at(&e, 6));
            set(&mut block, 1, 2, avg3_at(&e, 7));
            set(&mut block, 0, 1, avg2_at(&e, 7));
            set(&mut block, 3, 1, avg3_at(&e, 7));
            set(&mut block, 0, 2, avg2_at(&e, 7));
        }
        7 => {
            set(&mut block, 0, 0, avg2(p, a[0]));
            set(&mut block, 1, 0, avg3_at(&a, 1));
            set(&mut block, 0, 1, avg2_at(&a, 1));
            set(&mut block, 2, 0, avg3_at(&a, 2));
            set(&mut block, 1, 1, avg2_at(&a, 2));
            set(&mut block, 0, 2, avg3_at(&a, 2));
            set(&mut block, 3, 0, avg3_at(&a, 3));
            set(&mut block, 2, 1, avg2_at(&a, 3));
            set(&mut block, 1, 2, avg3_at(&a, 3));
            set(&mut block, 0, 3, avg2_at(&a, 3));
            set(&mut block, 3, 1, avg3_at(&a, 4));
            set(&mut block, 2, 2, avg2_at(&a, 4));
            set(&mut block, 1, 3, avg3_at(&a, 4));
            set(&mut block, 3, 2, avg3_at(&a, 5));
            set(&mut block, 2, 3, avg3_at(&a, 6));
            set(&mut block, 3, 3, avg3_at(&a, 7));
        }
        8 => {
            set(&mut block, 3, 0, avg2_at(&e, 0));
            set(&mut block, 3, 1, avg3_at(&e, 1));
            set(&mut block, 2, 0, avg2_at(&e, 1));
            set(&mut block, 3, 2, avg2_at(&e, 1));
            set(&mut block, 2, 1, avg3_at(&e, 2));
            set(&mut block, 3, 3, avg3_at(&e, 2));
            set(&mut block, 2, 2, avg2_at(&e, 2));
            set(&mut block, 1, 0, avg2_at(&e, 2));
            set(&mut block, 2, 3, avg3_at(&e, 3));
            set(&mut block, 1, 1, avg3_at(&e, 3));
            set(&mut block, 1, 2, avg2_at(&e, 3));
            set(&mut block, 0, 0, avg2_at(&e, 3));
            set(&mut block, 1, 3, avg3_at(&e, 4));
            set(&mut block, 0, 1, avg3_at(&e, 4));
            set(&mut block, 0, 2, avg3_at(&e, 5));
            set(&mut block, 0, 3, avg3_at(&e, 6));
        }
        9 => {
            set(&mut block, 0, 0, avg2_at(&l, 0));
            set(&mut block, 0, 1, avg3_at(&l, 1));
            set(&mut block, 0, 2, avg2_at(&l, 1));
            set(&mut block, 1, 0, avg2_at(&l, 1));
            set(&mut block, 0, 3, avg3_at(&l, 2));
            set(&mut block, 1, 1, avg3_at(&l, 2));
            set(&mut block, 1, 2, avg2_at(&l, 2));
            set(&mut block, 2, 0, avg2_at(&l, 2));
            set(&mut block, 1, 3, avg3(l[2], l[3], l[3]));
            set(&mut block, 2, 1, avg3(l[2], l[3], l[3]));
            for y in 2..4 {
                for x in 0..4 {
                    if y == 2 && x < 2 {
                        continue;
                    }
                    set(&mut block, x, y, l[3]);
                }
            }
        }
        _ => return Err(webp_err("invalid VP8 subblock prediction mode")),
    }
    Ok(block)
}

/// Adds a transformed residual to a prediction block and writes it into a
/// plane at the given top-left position.
#[allow(clippy::too_many_arguments)]
fn vp8_add_block(
    plane: &mut [u8],
    stride: usize,
    origin_x: usize,
    origin_y: usize,
    prediction: &[u8],
    prediction_x: usize,
    prediction_y: usize,
    prediction_stride: usize,
    coefficients: &[i32; 16],
) {
    let residual = vp8_inverse_dct(coefficients);
    for y in 0..4 {
        for x in 0..4 {
            let predicted = prediction[(prediction_y + y) * prediction_stride + prediction_x + x];
            plane[(origin_y + y) * stride + origin_x + x] =
                vp8_clamp(i32::from(predicted) + residual[y * 4 + x]);
        }
    }
}

#[derive(Clone, Copy)]
struct Vp8MbInfo {
    segment: usize,
    is_i4x4: bool,
    has_coefficients: bool,
}

/// Decodes all macroblocks and returns the three YUV planes (padded to whole
/// macroblocks) plus per-macroblock filter information.
type Vp8Planes = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<Vp8MbInfo>);

#[allow(clippy::needless_range_loop)]
fn vp8_reconstruct(
    frame: &Vp8Frame<'_>,
    mode_reader: &mut Vp8BoolReader<'_>,
) -> Result<Vp8Planes, DecodeError> {
    let mut readers = frame
        .partitions
        .iter()
        .map(|partition| Vp8BoolReader::new(partition))
        .collect::<Result<Vec<_>, _>>()?;

    let y_stride = frame.mb_width * 16;
    let uv_stride = frame.mb_width * 8;
    let mut y_plane = vec![0_u8; y_stride * frame.mb_height * 16];
    let mut u_plane = vec![0_u8; uv_stride * frame.mb_height * 8];
    let mut v_plane = vec![0_u8; uv_stride * frame.mb_height * 8];
    let mut mb_info = vec![
        Vp8MbInfo {
            segment: 0,
            is_i4x4: false,
            has_coefficients: false,
        };
        frame.mb_width * frame.mb_height
    ];

    let mut above_modes;
    let mut left_edge_modes = [0_u8; 4];
    let mut above_nz = vec![0_u16; frame.mb_width];
    let mut above_nz_dc = vec![false; frame.mb_width];

    for mb_y in 0..frame.mb_height {
        let mut left_nz = 0_u16;
        let mut left_nz_dc = false;
        above_modes = [0; 4];
        for mb_x in 0..frame.mb_width {
            // Macroblock header (section 11).
            let segment = if frame.segment_enabled {
                if mode_reader.read_bool(frame.segment_probs[0])? == 0 {
                    mode_reader.read_bool(frame.segment_probs[1])? as usize
                } else {
                    2 + mode_reader.read_bool(frame.segment_probs[2])? as usize
                }
            } else {
                0
            };
            let skip = frame.use_skip_proba && mode_reader.read_bool(frame.skip_p)? != 0;
            let is_i4x4 = mode_reader.read_bool(145)? == 0;
            let mut y_mode = 0_usize;
            let mut b_modes = [0_u8; 16];
            if is_i4x4 {
                for by in 0..4 {
                    for bx in 0..4 {
                        let above = if by == 0 {
                            above_modes[bx]
                        } else {
                            b_modes[(by - 1) * 4 + bx]
                        };
                        let left = if bx == 0 {
                            left_edge_modes[by]
                        } else {
                            b_modes[by * 4 + bx - 1]
                        };
                        let probs = &VP8_KBMODESPROBA[above as usize][left as usize];
                        let mode = if mode_reader.read_bool(probs[0])? == 0 {
                            0
                        } else if mode_reader.read_bool(probs[1])? == 0 {
                            1
                        } else if mode_reader.read_bool(probs[2])? == 0 {
                            2
                        } else if mode_reader.read_bool(probs[3])? == 0 {
                            if mode_reader.read_bool(probs[4])? == 0 {
                                3
                            } else if mode_reader.read_bool(probs[5])? == 0 {
                                5
                            } else {
                                6
                            }
                        } else if mode_reader.read_bool(probs[6])? == 0 {
                            4
                        } else if mode_reader.read_bool(probs[7])? == 0 {
                            7
                        } else if mode_reader.read_bool(probs[8])? == 0 {
                            8
                        } else {
                            9
                        };
                        b_modes[by * 4 + bx] = mode as u8;
                    }
                }
            } else {
                y_mode = if mode_reader.read_bool(156)? == 0 {
                    if mode_reader.read_bool(163)? == 0 {
                        0
                    } else {
                        1
                    }
                } else if mode_reader.read_bool(128)? == 0 {
                    2
                } else {
                    3
                };
                b_modes.fill(match y_mode {
                    0 => 0,
                    1 => 2,
                    2 => 3,
                    _ => 1,
                });
            }
            let uv_mode = if mode_reader.read_bool(142)? == 0 {
                0
            } else if mode_reader.read_bool(114)? == 0 {
                1
            } else if mode_reader.read_bool(183)? != 0 {
                3
            } else {
                2
            };

            let quant = &frame.quant[segment * 3..segment * 3 + 3];
            let partition_index = mb_y % readers.len();
            let token_reader = &mut readers[partition_index];
            let mut dc_values = [0_i32; 16];
            let mut y_coeffs = [[0_i32; 16]; 16];
            let mut uv_coeffs = [[0_i32; 16]; 8];
            let mut nonzero_y = false;
            let mut nonzero_uv = false;

            if skip {
                above_nz_dc[mb_x] = false;
                left_nz_dc = false;
                above_nz[mb_x] = 0;
                left_nz = 0;
            } else {
                let mut new_above_nz = above_nz[mb_x];
                let mut new_left_nz = 0_u16;
                let first = if is_i4x4 { 0 } else { 1 };
                if !is_i4x4 {
                    let context = usize::from(above_nz_dc[mb_x]) + usize::from(left_nz_dc);
                    let mut block = [0_i32; 16];
                    let n = vp8_read_coeffs(
                        token_reader,
                        &frame.token_probs,
                        1,
                        quant[1],
                        context,
                        0,
                        &mut block,
                    )?;
                    if n > 0 {
                        above_nz_dc[mb_x] = true;
                        left_nz_dc = true;
                    }
                    if n > 1 {
                        dc_values = vp8_inverse_wht(&block);
                    } else {
                        dc_values.fill((block[0] + 3) >> 3);
                    }
                }
                for by in 0..4 {
                    let mut left_context = (left_nz >> (by * 4)) & 1;
                    for bx in 0..4 {
                        let subblock = by * 4 + bx;
                        let context =
                            left_context as usize + usize::from((above_nz[mb_x] >> subblock) & 1);
                        let n = vp8_read_coeffs(
                            token_reader,
                            &frame.token_probs,
                            if is_i4x4 { 3 } else { 0 },
                            quant[0],
                            context,
                            first,
                            &mut y_coeffs[subblock],
                        )?;
                        if !is_i4x4 {
                            y_coeffs[subblock][0] = dc_values[subblock];
                        }
                        left_context = u16::from(n > first);
                        let bit = left_context << subblock;
                        new_above_nz = (new_above_nz & !(1_u16 << subblock)) | bit;
                        new_left_nz |= bit;
                        nonzero_y |= n > first;
                    }
                }
                for plane_index in 0..2 {
                    let high_bit = 4 + plane_index * 2;
                    for by in 0..2 {
                        let mut left_context = (left_nz >> (high_bit + by * 2)) & 1;
                        for bx in 0..2 {
                            let block_index = by * 2 + bx;
                            let bit_position = high_bit + block_index;
                            let context = left_context as usize
                                + usize::from((above_nz[mb_x] >> bit_position) & 1);
                            let n = vp8_read_coeffs(
                                token_reader,
                                &frame.token_probs,
                                2,
                                quant[2],
                                context,
                                0,
                                &mut uv_coeffs[plane_index * 4 + block_index],
                            )?;
                            left_context = u16::from(n > 0);
                            let bit = left_context << bit_position;
                            new_above_nz = (new_above_nz & !(1_u16 << bit_position)) | bit;
                            new_left_nz |= bit;
                            nonzero_uv |= n > 0;
                        }
                    }
                }
                above_nz[mb_x] = new_above_nz;
                left_nz = new_left_nz;
            }

            // Intra prediction and reconstruction.
            let origin_x = mb_x * 16;
            let origin_y = mb_y * 16;
            if is_i4x4 {
                for by in 0..4 {
                    for bx in 0..4 {
                        let subblock = by * 4 + bx;
                        let prediction = vp8_subblock_predict(
                            &y_plane,
                            y_stride,
                            mb_x,
                            mb_y,
                            bx,
                            by,
                            usize::from(b_modes[subblock]),
                        )?;
                        vp8_add_block(
                            &mut y_plane,
                            y_stride,
                            origin_x + bx * 4,
                            origin_y + by * 4,
                            &prediction,
                            0,
                            0,
                            4,
                            &y_coeffs[subblock],
                        );
                    }
                }
            } else {
                let prediction = vp8_intra_predict(&y_plane, y_stride, mb_x, mb_y, 16, y_mode)?;
                for subblock in 0..16 {
                    let bx = subblock % 4;
                    let by = subblock / 4;
                    vp8_add_block(
                        &mut y_plane,
                        y_stride,
                        origin_x + bx * 4,
                        origin_y + by * 4,
                        &prediction,
                        bx * 4,
                        by * 4,
                        16,
                        &y_coeffs[subblock],
                    );
                }
            }
            for plane_index in 0..2 {
                let (plane, coefficients) = if plane_index == 0 {
                    (&mut u_plane, &uv_coeffs[0..4])
                } else {
                    (&mut v_plane, &uv_coeffs[4..8])
                };
                let prediction = vp8_intra_predict(plane, uv_stride, mb_x, mb_y, 8, uv_mode)?;
                for block_index in 0..4 {
                    let bx = block_index % 2;
                    let by = block_index / 2;
                    vp8_add_block(
                        plane,
                        uv_stride,
                        mb_x * 8 + bx * 4,
                        mb_y * 8 + by * 4,
                        &prediction,
                        bx * 4,
                        by * 4,
                        8,
                        &coefficients[block_index],
                    );
                }
            }

            mb_info[mb_y * frame.mb_width + mb_x] = Vp8MbInfo {
                segment,
                is_i4x4,
                has_coefficients: nonzero_y || nonzero_uv,
            };
            above_modes = if is_i4x4 {
                [b_modes[12], b_modes[13], b_modes[14], b_modes[15]]
            } else {
                [b_modes[0], b_modes[0], b_modes[0], b_modes[0]]
            };
            left_edge_modes = if is_i4x4 {
                [b_modes[3], b_modes[7], b_modes[11], b_modes[15]]
            } else {
                [b_modes[0], b_modes[0], b_modes[0], b_modes[0]]
            };
        }
    }
    Ok((y_plane, u_plane, v_plane, mb_info))
}

// -------------------------------------------------------------------------
// Loop filter
// -------------------------------------------------------------------------

fn vp8_abs_difference(a: u8, b: u8) -> i32 {
    (i32::from(a) - i32::from(b)).abs()
}

fn vp8_simple_threshold(
    plane: &[u8],
    position: usize,
    stride: usize,
    limit: u16,
) -> Result<bool, DecodeError> {
    let p1 = *plane
        .get(
            position
                .checked_sub(2 * stride)
                .ok_or_else(|| webp_err("loop filter out of bounds"))?,
        )
        .ok_or_else(|| webp_err("loop filter out of bounds"))?;
    let p0 = plane[position - stride];
    let q0 = plane[position];
    let q1 = *plane
        .get(position + stride)
        .ok_or_else(|| webp_err("loop filter out of bounds"))?;
    Ok(vp8_abs_difference(p0, q0) * 2 + vp8_abs_difference(p1, q1) / 2 <= i32::from(limit))
}

#[allow(clippy::too_many_arguments)]
fn vp8_normal_threshold(
    plane: &[u8],
    position: usize,
    stride: usize,
    edge: u16,
    interior: u8,
) -> Result<bool, DecodeError> {
    let index = |offset: isize| -> Result<usize, DecodeError> {
        let value = position as isize + offset * stride as isize;
        if value < 0 {
            return Err(webp_err("loop filter out of bounds"));
        }
        Ok(value as usize)
    };
    let p3 = plane[index(-4)?];
    let p2 = plane[index(-3)?];
    let p1 = plane[index(-2)?];
    let p0 = plane[index(-1)?];
    let q0 = plane[index(0)?];
    let q1 = plane[index(1)?];
    let q2 = plane[index(2)?];
    let q3 = plane[index(3)?];
    let interior = i32::from(interior);
    let effective_edge = 2 * i32::from(edge) + interior;
    Ok(
        vp8_abs_difference(p0, q0) * 2 + vp8_abs_difference(p1, q1) / 2 <= effective_edge
            && vp8_abs_difference(p3, p2) <= interior
            && vp8_abs_difference(p2, p1) <= interior
            && vp8_abs_difference(p1, p0) <= interior
            && vp8_abs_difference(q3, q2) <= interior
            && vp8_abs_difference(q2, q1) <= interior
            && vp8_abs_difference(q1, q0) <= interior,
    )
}

fn vp8_high_edge_variance(
    plane: &[u8],
    position: usize,
    stride: usize,
    threshold: u8,
) -> Result<bool, DecodeError> {
    let p0 = plane[position - stride];
    let p1 = plane[position - 2 * stride];
    let q0 = plane[position];
    let q1 = plane[position + stride];
    Ok(vp8_abs_difference(p1, p0) > i32::from(threshold)
        || vp8_abs_difference(q1, q0) > i32::from(threshold))
}

fn vp8_filter_common(
    plane: &mut [u8],
    position: usize,
    stride: usize,
    use_outer_taps: bool,
) -> Result<(), DecodeError> {
    let p1 = plane[position - 2 * stride];
    let p0 = plane[position - stride];
    let q0 = plane[position];
    let q1 = plane[position + stride];
    let mut a = 3 * (i32::from(q0) - i32::from(p0));
    if use_outer_taps {
        a += vp8_saturate_i8(i32::from(p1) - i32::from(q1));
    }
    let a = vp8_saturate_i8(a);
    let q_adjust = ((a + 4).min(127)) >> 3;
    let p_adjust = ((a + 3).min(127)) >> 3;
    plane[position - stride] = vp8_clamp(i32::from(p0) + p_adjust);
    plane[position] = vp8_clamp(i32::from(q0) - q_adjust);
    if !use_outer_taps {
        let inner = (q_adjust + 1) >> 1;
        plane[position - 2 * stride] = vp8_clamp(i32::from(p1) + inner);
        plane[position + stride] = vp8_clamp(i32::from(q1) - inner);
    }
    Ok(())
}

fn vp8_filter_mb_edge(plane: &mut [u8], position: usize, stride: usize) {
    let p1 = plane[position - 2 * stride];
    let p0 = plane[position - stride];
    let q0 = plane[position];
    let q1 = plane[position + stride];
    let p2 = plane[position - 3 * stride];
    let q2 = plane[position + 2 * stride];
    let w = vp8_saturate_i8(
        vp8_saturate_i8(i32::from(p1) - i32::from(q1)) + 3 * (i32::from(q0) - i32::from(p0)),
    );
    for distance in 0..3 {
        let numerator = match distance {
            0 => 27,
            1 => 18,
            _ => 9,
        };
        let adjustment = (numerator * w + 63) >> 7;
        match distance {
            0 => {
                plane[position - stride] = vp8_clamp(i32::from(p0) + adjustment);
                plane[position] = vp8_clamp(i32::from(q0) - adjustment);
            }
            1 => {
                plane[position - 2 * stride] = vp8_clamp(i32::from(p1) + adjustment);
                plane[position + stride] = vp8_clamp(i32::from(q1) - adjustment);
            }
            _ => {
                plane[position - 3 * stride] = vp8_clamp(i32::from(p2) + adjustment);
                plane[position + 2 * stride] = vp8_clamp(i32::from(q2) - adjustment);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn vp8_filter_segment(
    plane: &mut [u8],
    position: usize,
    stride: usize,
    strength: &Vp8FilterStrength,
    mb_edge: bool,
    simple: bool,
) -> Result<(), DecodeError> {
    if strength.level == 0 {
        return Ok(());
    }
    let edge = u16::from(strength.level) + if mb_edge { 2 } else { 0 };
    let limit = 2 * edge + u16::from(strength.interior);
    if simple {
        if vp8_simple_threshold(plane, position, stride, limit)? {
            vp8_filter_common(plane, position, stride, true)?;
        }
    } else if vp8_normal_threshold(plane, position, stride, edge, strength.interior)? {
        if vp8_high_edge_variance(plane, position, stride, strength.hev)? {
            vp8_filter_common(plane, position, stride, true)?;
        } else if mb_edge {
            vp8_filter_mb_edge(plane, position, stride);
        } else {
            vp8_filter_common(plane, position, stride, false)?;
        }
    }
    Ok(())
}

fn vp8_loop_filter(
    frame: &Vp8Frame<'_>,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    mb_info: &[Vp8MbInfo],
    simple: bool,
) -> Result<(), DecodeError> {
    let y_stride = frame.mb_width * 16;
    let uv_stride = frame.mb_width * 8;
    for mb_y in 0..frame.mb_height {
        for mb_x in 0..frame.mb_width {
            let info = mb_info[mb_y * frame.mb_width + mb_x];
            let strength = frame.filters[info.segment][usize::from(info.is_i4x4)];
            if strength.level == 0 {
                continue;
            }
            let filter_inner = info.is_i4x4 || info.has_coefficients;
            let y_base = mb_y * 16 * y_stride + mb_x * 16;
            let u_base = mb_y * 8 * uv_stride + mb_x * 8;
            let v_base = u_base;
            if mb_x > 0 {
                for row in 0..16 {
                    vp8_filter_segment(y, y_base + row * y_stride, 1, &strength, true, simple)?;
                }
                if !simple {
                    for row in 0..8 {
                        vp8_filter_segment(u, u_base + row * uv_stride, 1, &strength, true, false)?;
                        vp8_filter_segment(v, v_base + row * uv_stride, 1, &strength, true, false)?;
                    }
                }
            }
            if filter_inner {
                for offset in [4, 8, 12] {
                    for row in 0..16 {
                        vp8_filter_segment(
                            y,
                            y_base + row * y_stride + offset,
                            1,
                            &strength,
                            false,
                            simple,
                        )?;
                    }
                }
                if !simple {
                    for row in 0..8 {
                        vp8_filter_segment(
                            u,
                            u_base + row * uv_stride + 4,
                            1,
                            &strength,
                            false,
                            false,
                        )?;
                        vp8_filter_segment(
                            v,
                            v_base + row * uv_stride + 4,
                            1,
                            &strength,
                            false,
                            false,
                        )?;
                    }
                }
            }
            if mb_y > 0 {
                for column in 0..16 {
                    vp8_filter_segment(y, y_base + column, y_stride, &strength, true, simple)?;
                }
                if !simple {
                    for column in 0..8 {
                        vp8_filter_segment(u, u_base + column, uv_stride, &strength, true, false)?;
                        vp8_filter_segment(v, v_base + column, uv_stride, &strength, true, false)?;
                    }
                }
            }
            if filter_inner {
                for offset in [4, 8, 12] {
                    for column in 0..16 {
                        vp8_filter_segment(
                            y,
                            y_base + offset * y_stride + column,
                            y_stride,
                            &strength,
                            false,
                            simple,
                        )?;
                    }
                }
                if !simple {
                    for column in 0..8 {
                        vp8_filter_segment(
                            u,
                            u_base + 4 * uv_stride + column,
                            uv_stride,
                            &strength,
                            false,
                            false,
                        )?;
                        vp8_filter_segment(
                            v,
                            v_base + 4 * uv_stride + column,
                            uv_stride,
                            &strength,
                            false,
                            false,
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn vp8_scale_dimension(size: u32, scale: u32) -> u32 {
    let (numerator, denominator) = match scale {
        1 => (5_u64, 4_u64),
        2 => (5, 3),
        3 => (2, 1),
        _ => return size,
    };
    (u64::from(size) * numerator).div_ceil(denominator) as u32
}

/// YCbCr to RGB using libwebp's bit-exact fixed-point BT.601 conversion.
fn vp8_yuv_to_rgba(y: u8, u: u8, v: u8) -> [u8; 4] {
    fn clip(value: i32) -> u8 {
        if value & !((256 << 6) - 1) == 0 {
            (value >> 6) as u8
        } else if value < 0 {
            0
        } else {
            255
        }
    }
    let red = clip(((i32::from(y) * 19077) >> 8) + ((i32::from(v) * 26149) >> 8) - 14234);
    let green = clip(
        ((i32::from(y) * 19077) >> 8)
            - ((i32::from(u) * 6419) >> 8)
            - ((i32::from(v) * 13320) >> 8)
            + 8708,
    );
    let blue = clip(((i32::from(y) * 19077) >> 8) + ((i32::from(u) * 33050) >> 8) - 17685);
    [red, green, blue, 255]
}

/// Decodes a single VP8 key frame (the payload of a `VP8 ` chunk) into RGBA8.
#[allow(unreachable_code, unused_variables)]
fn decode_vp8_keyframe(data: &[u8]) -> Result<(u32, u32, Vec<u8>), DecodeError> {
    // The VP8 key-frame pipeline below parses headers, modes, tokens and
    // transforms but is not yet bit-exact with libwebp's reconstruction, so
    // it is kept dormant rather than returning incorrect pixels.
    return Err(webp_unsupported(
        "VP8 lossy key frames (VP8L lossless is supported)",
    ));
    let (frame, mut mode_reader) = vp8_parse_frame(data)?;
    let (mut y, mut u, mut v, mb_info) = vp8_reconstruct(&frame, &mut mode_reader)?;
    // A zero loop-filter level disables filtering entirely (handled per
    // segment inside the filter itself).
    vp8_loop_filter(
        &frame,
        &mut y,
        &mut u,
        &mut v,
        &mb_info,
        frame.simple_filter,
    )?;

    let out_width = vp8_scale_dimension(frame.width, frame.scale_h);
    let out_height = vp8_scale_dimension(frame.height, frame.scale_v);
    check_pixel_count(out_width, out_height, "VP8")?;
    let y_stride = frame.mb_width * 16;
    let uv_stride = frame.mb_width * 8;
    let mut pixels = Vec::with_capacity(out_width as usize * out_height as usize * 4);
    for out_y in 0..out_height as usize {
        let source_y = out_y * frame.height as usize / out_height as usize;
        for out_x in 0..out_width as usize {
            let source_x = out_x * frame.width as usize / out_width as usize;
            let luma = y[source_y * y_stride + source_x];
            let chroma = u[(source_y / 2) * uv_stride + source_x / 2];
            let chroma_v = v[(source_y / 2) * uv_stride + source_x / 2];
            pixels.extend_from_slice(&vp8_yuv_to_rgba(luma, chroma, chroma_v));
        }
    }
    Ok((out_width, out_height, pixels))
}

// -------------------------------------------------------------------------
// RIFF / WebP container
// -------------------------------------------------------------------------

struct WebpChunk<'a> {
    fourcc: [u8; 4],
    data: &'a [u8],
}

fn webp_chunks(bytes: &[u8]) -> Result<Vec<WebpChunk<'_>>, DecodeError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(webp_err("missing RIFF/WEBP header"));
    }
    let declared = webp_u32le(bytes, 4)? as usize;
    let end = 8_usize
        .checked_add(declared)
        .map(|value| value.min(bytes.len()))
        .ok_or_else(|| webp_err("RIFF size overflow"))?;
    let mut chunks = Vec::new();
    let mut position = 12_usize;
    while position + 8 <= end {
        let fourcc: [u8; 4] = bytes[position..position + 4]
            .try_into()
            .map_err(|_| webp_err("invalid RIFF chunk header"))?;
        let size = webp_u32le(bytes, position + 4)? as usize;
        let data_start = position + 8;
        let data_end = data_start
            .checked_add(size)
            .ok_or_else(|| webp_err("RIFF chunk size overflow"))?;
        if data_end > end {
            return Err(webp_err("truncated RIFF chunk payload"));
        }
        chunks.push(WebpChunk {
            fourcc,
            data: &bytes[data_start..data_end],
        });
        position = data_end + (size & 1);
    }
    Ok(chunks)
}

fn webp_decode_alpha(alpha: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DecodeError> {
    let flags = *alpha.first().ok_or_else(|| webp_err("empty ALPH chunk"))?;
    let compression = flags & 0x03;
    let filter = (flags >> 2) & 0x03;
    let payload = &alpha[1..];
    let count = width
        .checked_mul(height)
        .ok_or_else(|| webp_err("WebP image size overflow"))?;
    let mut values = match compression {
        0 => {
            if payload.len() < count {
                return Err(webp_err("truncated raw ALPH data"));
            }
            payload[..count].to_vec()
        }
        1 => {
            let mut reader = Vp8LReader::new(payload);
            let argb = vp8l_decode_stream(&mut reader, width, height, false)?;
            argb.iter().map(|pixel| pixel[2]).collect()
        }
        _ => return Err(webp_unsupported("ALPH compression mode")),
    };
    if filter > 3 {
        return Err(webp_err("invalid ALPH filter method"));
    }
    let mut previous = vec![0_u8; width];
    for y in 0..height {
        for x in 0..width {
            let position = y * width + x;
            let predictor = match filter {
                0 => 0_u8,
                1 => {
                    if x > 0 {
                        values[position - 1]
                    } else if y > 0 {
                        previous[x]
                    } else {
                        0
                    }
                }
                2 => {
                    if y > 0 {
                        previous[x]
                    } else if x > 0 {
                        values[position - 1]
                    } else {
                        0
                    }
                }
                _ => {
                    let left = if x > 0 { values[position - 1] } else { 0 };
                    let above = if y > 0 { previous[x] } else { 0 };
                    let above_left = if x > 0 && y > 0 { previous[x - 1] } else { 0 };
                    (i32::from(left) + i32::from(above) - i32::from(above_left)).clamp(0, 255) as u8
                }
            };
            values[position] = values[position].wrapping_add(predictor);
        }
        previous.copy_from_slice(&values[y * width..(y + 1) * width]);
    }
    Ok(values)
}

/// Decodes a WebP file into RGBA8, dispatching on the bitstream chunk.
pub fn decode_webp(bytes: &[u8]) -> Result<ImageData, DecodeError> {
    let chunks = webp_chunks(bytes)?;
    let mut extended: Option<[u8; 10]> = None;
    let mut alpha: Option<&[u8]> = None;
    let mut bitstream: Option<(&[u8; 4], &[u8])> = None;
    for chunk in &chunks {
        match &chunk.fourcc {
            b"VP8X" => {
                let header: [u8; 10] = chunk
                    .data
                    .get(..10)
                    .ok_or_else(|| webp_err("truncated VP8X chunk"))?
                    .try_into()
                    .map_err(|_| webp_err("invalid VP8X chunk"))?;
                extended = Some(header);
            }
            b"ALPH" => alpha = Some(chunk.data),
            b"VP8 " | b"VP8L" => {
                if bitstream.is_some() {
                    return Err(webp_err("multiple bitstream chunks"));
                }
                bitstream = Some((&chunk.fourcc, chunk.data));
            }
            b"ANIM" | b"ANMF" => return Err(webp_unsupported("animated WebP images")),
            _ => {}
        }
    }
    let (fourcc, payload) =
        bitstream.ok_or_else(|| webp_err("WebP file has no VP8 or VP8L bitstream"))?;

    let (mut width, mut height, mut pixels) = if fourcc == b"VP8L" {
        decode_vp8l(payload)?
    } else {
        decode_vp8_keyframe(payload)?
    };

    if let Some(header) = extended {
        if header[0] & 0x02 != 0 {
            return Err(webp_unsupported("animated WebP images"));
        }
        let canvas_width = webp_u24le(&header, 4)? + 1;
        let canvas_height = webp_u24le(&header, 7)? + 1;
        if canvas_width > MAX_DIMENSION || canvas_height > MAX_DIMENSION {
            return Err(webp_err("WebP canvas exceeds the dimension limit"));
        }
        check_pixel_count(canvas_width, canvas_height, "WebP")?;
        if canvas_width != width || canvas_height != height {
            return Err(webp_unsupported(
                "WebP bitstream scaled to a different canvas",
            ));
        }
        width = canvas_width;
        height = canvas_height;
    }

    if let Some(alpha_data) = alpha {
        if fourcc == b"VP8L" {
            return Err(webp_err("VP8L images must not carry an ALPH chunk"));
        }
        let values = webp_decode_alpha(alpha_data, width as usize, height as usize)?;
        for (pixel, value) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            pixel[3] = value;
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

    fn webp_base64(text: &str) -> Vec<u8> {
        fn digit(byte: u8) -> Option<u8> {
            match byte {
                b'A'..=b'Z' => Some(byte - b'A'),
                b'a'..=b'z' => Some(byte - b'a' + 26),
                b'0'..=b'9' => Some(byte - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                b'=' => None,
                _ => panic!("invalid base64 digit"),
            }
        }
        let mut accumulator = 0_u32;
        let mut bits = 0_u32;
        let mut output = Vec::new();
        for byte in text.bytes() {
            if let Some(value) = digit(byte) {
                accumulator = (accumulator << 6) | u32::from(value);
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    output.push((accumulator >> bits) as u8);
                }
            }
        }
        output
    }

    /// 16x16 VP8L fixture produced by cwebp from an RGBA gradient.
    const WEBP_VP8L: &str =
        "UklGRiwAAABXRUJQVlA4TCAAAAAvD8ADALmM6H/sClGIgvc/ICQgTPD/sKgORMQEgNW7AA==";

    /// 23x17 VP8 key-frame fixture produced by cwebp.
    const WEBP_VP8: &str = "UklGRoAAAABXRUJQVlA4IHQAAABQBACdASoXABEAPoUwlUelIqIhN/qoAKAQiWgAnTKEePe6AGBRfc5xFcsAAP6+cvySZ+ADTahToYPV5Z+3TkcdOXfX/6Vz+Vi+/8nkljuhiHijC03vTe4+EleydX3L1W7b8Ws3zLXC0V0lG76MnUn0hyAAAA==";

    #[test]
    fn webp_detect_and_decode_vp8l() {
        let bytes = webp_base64(WEBP_VP8L);
        assert_eq!(detect_format(&bytes), Some(ImageFormat::Webp));
        let image = decode(&bytes).expect("VP8L fixture decodes");
        assert_eq!((image.width, image.height), (16, 16));
        assert_eq!(image.pixels.len(), 16 * 16 * 4);
        // Lossless decoding reproduces the source gradient exactly.
        for y in 0..16_usize {
            for x in 0..16_usize {
                let offset = (y * 16 + x) * 4;
                assert_eq!(
                    &image.pixels[offset..offset + 4],
                    &[(x * 16) as u8, (y * 16) as u8, ((x + y) * 8) as u8, 255,]
                );
            }
        }
    }

    #[test]
    fn webp_decode_vp8_keyframe() {
        let bytes = webp_base64(WEBP_VP8);
        let error = decode(&bytes).expect_err("VP8 lossy is not finished");
        assert!(
            error.message.contains("unsupported image data"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn webp_malformed_data_returns_error() {
        let fixture = webp_base64(WEBP_VP8L);
        assert!(decode(&[]).is_err());
        assert!(decode(b"RIFF").is_err());
        assert!(decode(&fixture[..fixture.len() - 1]).is_err());
        // Truncated and randomly mutated WebP data must never panic.
        let mut state = 0x1234_5678_u32;
        for _ in 0..64 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let mut mutated = fixture.clone();
            let index = (state as usize) % mutated.len();
            mutated[index] ^= (state >> 16) as u8;
            let _ = decode(&mutated);
            let cut = (state >> 8) as usize % mutated.len();
            let _ = decode(&mutated[..cut]);
        }
    }
}
