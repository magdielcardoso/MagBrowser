//! In-memory RGBA images and nearest-neighbour scaling.

use crate::decode::{DecodeError, decode};

/// A decoded RGBA8 image.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageData {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA pixels, row-major.
    pub pixels: Vec<u8>,
}

impl ImageData {
    /// Creates a transparent image.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    /// Builds an image from a caller-provided RGBA8 buffer.
    ///
    /// The buffer length must be exactly `width * height * 4` bytes.
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, DecodeError> {
        let expected = (width as u64)
            .checked_mul(height as u64)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| DecodeError::new("image dimensions overflow"))?;
        if expected != pixels.len() as u64 {
            return Err(DecodeError::new(format!(
                "RGBA buffer has {} bytes but {}x{} needs {expected}",
                pixels.len(),
                width,
                height
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Decodes encoded image bytes (PNG or BMP) into RGBA8.
    ///
    /// See [`crate::decode`] for the supported formats.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        decode(bytes)
    }

    /// A deterministic checkerboard placeholder used for images that could not
    /// be decoded (or that have no source yet).
    pub fn placeholder(width: u32, height: u32) -> Self {
        let mut image = Self::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let cell = ((x / 8) + (y / 8)) % 2;
                let (r, g, b) = if cell == 0 {
                    (0xdd, 0xdd, 0xdd)
                } else {
                    (0xbb, 0xbb, 0xbb)
                };
                image.set(x, y, r, g, b, 255);
            }
        }
        image
    }

    /// Sets one pixel.
    pub fn set(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        self.pixels[index] = r;
        self.pixels[index + 1] = g;
        self.pixels[index + 2] = b;
        self.pixels[index + 3] = a;
    }

    /// Reads one pixel as RGBA.
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        [
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ]
    }

    /// Samples the image with nearest-neighbour filtering.
    pub fn sample(&self, u: f32, v: f32) -> [u8; 4] {
        if self.width == 0 || self.height == 0 {
            return [0, 0, 0, 0];
        }
        let x = (u.clamp(0.0, 0.999_999) * self.width as f32) as u32;
        let y = (v.clamp(0.0, 0.999_999) * self.height as f32) as u32;
        self.get(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_has_content_and_alternates() {
        let image = ImageData::placeholder(16, 16);
        assert_eq!(image.width, 16);
        assert_ne!(image.get(0, 0), image.get(8, 0));
        assert_eq!(image.get(0, 0)[3], 255);
    }

    #[test]
    fn sampling_is_clamped() {
        let mut image = ImageData::new(2, 2);
        image.set(1, 1, 1, 2, 3, 255);
        assert_eq!(image.sample(0.75, 0.75), [1, 2, 3, 255]);
        assert_eq!(image.sample(2.0, 2.0), [1, 2, 3, 255]);
    }
}
