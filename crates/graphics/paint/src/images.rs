//! In-memory RGBA images, nearest-neighbour scaling and object fitting.

use browser_layout::Rect;
use browser_style::ObjectFit;

use crate::decode::{DecodeError, decode};

/// Largest accepted width (and height) of an in-memory image, in pixels.
///
/// Constructors clamp to this limit *before* allocating the backing buffer so a
/// hostile declaration such as `100000x100000` cannot trigger a multi-gigabyte
/// allocation. `4096x4096` RGBA8 is already 64 MiB, which is a reasonable upper
/// bound for a single decoded image.
pub const MAX_IMAGE_DIMENSION: u32 = 4096;

/// Rejects image dimensions that would produce an oversized allocation.
///
/// Used by fallible constructors (for example [`ImageData::from_rgba`]) so a
/// decoder reports a clean error instead of allocating. Infallible constructors
/// such as [`ImageData::new`] clamp with [`clamp_dimension`] instead.
pub fn check_dimensions(width: u32, height: u32) -> Result<(), DecodeError> {
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(DecodeError::new(format!(
            "image dimensions {width}x{height} exceed the {MAX_IMAGE_DIMENSION} pixel limit"
        )));
    }
    Ok(())
}

/// Clamps a single image edge to [`MAX_IMAGE_DIMENSION`].
pub fn clamp_dimension(value: u32) -> u32 {
    value.min(MAX_IMAGE_DIMENSION)
}

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
    ///
    /// Dimensions larger than [`MAX_IMAGE_DIMENSION`] are clamped so the backing
    /// buffer can never exceed 64 MiB.
    pub fn new(width: u32, height: u32) -> Self {
        let width = clamp_dimension(width);
        let height = clamp_dimension(height);
        Self {
            width,
            height,
            pixels: vec![
                0;
                (width as usize)
                    .saturating_mul(height as usize)
                    .saturating_mul(4)
            ],
        }
    }

    /// Builds an image from a caller-provided RGBA8 buffer.
    ///
    /// The buffer length must be exactly `width * height * 4` bytes.
    ///
    /// Returns an error when either edge exceeds [`MAX_IMAGE_DIMENSION`], before
    /// inspecting the buffer, so oversized decoded images fail gracefully.
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, DecodeError> {
        check_dimensions(width, height)?;
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

    /// Decodes encoded image bytes into RGBA8.
    ///
    /// Supported containers are PNG, GIF, BMP, binary PPM/PGM (`P5`/`P6`),
    /// PNG-backed ICO and SVG (rasterized by [`crate::svg`]). WebP and AVIF
    /// return a [`DecodeError`] describing why they are unsupported; see
    /// [`crate::decode`] for details. Dimensions are capped by
    /// [`MAX_IMAGE_DIMENSION`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        decode(bytes)
    }

    /// A fully transparent image of the given size.
    ///
    /// Unlike [`ImageData::placeholder`], this paints nothing at all, which is
    /// what decorative images (icons, background artwork, failed inline SVGs)
    /// want: no checkerboard and no opaque fallback. Dimensions are clamped to
    /// [`MAX_IMAGE_DIMENSION`] just like [`ImageData::new`].
    pub fn transparent(width: u32, height: u32) -> Self {
        Self::new(width, height)
    }

    /// A deterministic checkerboard placeholder used for images that could not
    /// be decoded (or that have no source yet).
    ///
    /// A cached 16×16 tile (two 8×8 cells per axis, matching the classic
    /// checkerboard) is repeated across the surface with row-wise block copies
    /// instead of calling [`Self::set`] once per pixel. Dimensions are clamped
    /// by [`Self::new`] before any work, so an oversized declaration cannot make
    /// this loop over billions of pixels.
    pub fn placeholder(width: u32, height: u32) -> Self {
        const TILE: usize = 16;
        const CELL: usize = 8;
        // One cached 16-pixel row of RGBA bytes for each of the 16 tile rows.
        static TILE_ROWS: std::sync::OnceLock<[[u8; TILE * 4]; TILE]> = std::sync::OnceLock::new();
        let rows = TILE_ROWS.get_or_init(|| {
            let mut rows = [[0u8; TILE * 4]; TILE];
            for (y, row) in rows.iter_mut().enumerate() {
                for x in 0..TILE {
                    let cell = ((x / CELL) + (y / CELL)) % 2;
                    let shade = if cell == 0 { 0xdd } else { 0xbb };
                    row[x * 4..x * 4 + 4].copy_from_slice(&[shade, shade, shade, 255]);
                }
            }
            rows
        });

        let mut image = Self::new(width, height);
        let (w, h) = (image.width as usize, image.height as usize);
        for y in 0..h {
            let pattern = &rows[y % TILE];
            let row_start = y * w * 4;
            let mut x = 0;
            while x + TILE <= w {
                image.pixels[row_start + x * 4..row_start + (x + TILE) * 4]
                    .copy_from_slice(pattern);
                x += TILE;
            }
            while x < w {
                let offset = (x % TILE) * 4;
                image.pixels[row_start + x * 4..row_start + x * 4 + 4]
                    .copy_from_slice(&pattern[offset..offset + 4]);
                x += 1;
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

/// A CSS `<length-percentage>` used by `object-position` / `background-position`.
///
/// Percentages are stored as fractions (`0.5` means `50%`). Negative lengths are
/// allowed: for *position* they are measured from the far edge of the box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthOrPercentage {
    /// A percentage of the available space, stored as a fraction (`0.5` = `50%`).
    Percentage(f32),
    /// An absolute length in CSS pixels.
    Length(f32),
}

impl LengthOrPercentage {
    /// Resolves this value against `available` free space along one axis.
    ///
    /// A percentage resolves to `fraction * available`; a non-negative length is
    /// measured from the start edge, and a negative length is measured from the
    /// end edge (matching CSS `object-position` / `background-position`).
    pub fn resolve(self, available: f32) -> f32 {
        match self {
            Self::Percentage(fraction) => fraction * available,
            Self::Length(length) => {
                if length >= 0.0 {
                    length
                } else {
                    available + length
                }
            }
        }
    }
}

/// Position of fitted content within its box (CSS `object-position` /
/// `background-position`).
///
/// The default is `center center`, i.e. both axes at `50%`. A single-value CSS
/// declaration leaves the unspecified axis at `50%`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImagePosition {
    /// Horizontal component.
    pub x: LengthOrPercentage,
    /// Vertical component.
    pub y: LengthOrPercentage,
}

/// The CSS `object-position` of a replaced element.
pub type ObjectPosition = ImagePosition;

impl Default for ImagePosition {
    fn default() -> Self {
        Self::center()
    }
}

impl ImagePosition {
    /// `center center` (both axes at `50%`).
    pub const fn center() -> Self {
        Self {
            x: LengthOrPercentage::Percentage(0.5),
            y: LengthOrPercentage::Percentage(0.5),
        }
    }

    /// Builds a position from two percentages given as fractions (`0.5` = `50%`).
    pub const fn from_percentages(x: f32, y: f32) -> Self {
        Self {
            x: LengthOrPercentage::Percentage(x),
            y: LengthOrPercentage::Percentage(y),
        }
    }

    /// Builds a position from two lengths in CSS pixels.
    pub const fn from_lengths(x: f32, y: f32) -> Self {
        Self {
            x: LengthOrPercentage::Length(x),
            y: LengthOrPercentage::Length(y),
        }
    }
}

/// Source and destination geometry for one fitted image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FittedImage {
    /// Part of the source image to display, in source pixel coordinates.
    ///
    /// With the standard `object-fit` / `background-size` models this is always
    /// the whole image; cropping happens by clipping [`Self::destination`].
    pub source: Rect,
    /// The same region in normalized `(u, v)` coordinates (`0.0..=1.0`),
    /// suitable for [`ImageData::sample`].
    pub source_uv: Rect,
    /// Where the source region is drawn, in the same coordinate space as the
    /// containing box.
    ///
    /// For `cover` and `none` (and oversized backgrounds) this may extend beyond
    /// the box; clip to the box while painting.
    pub destination: Rect,
}

impl FittedImage {
    /// Builds a fit from a source rectangle, the source's intrinsic size and a
    /// destination rectangle, deriving the normalized source rectangle.
    pub fn new(
        source: Rect,
        intrinsic_width: u32,
        intrinsic_height: u32,
        destination: Rect,
    ) -> Self {
        let inv_w = if intrinsic_width == 0 {
            0.0
        } else {
            1.0 / intrinsic_width as f32
        };
        let inv_h = if intrinsic_height == 0 {
            0.0
        } else {
            1.0 / intrinsic_height as f32
        };
        let source_uv = Rect::new(
            source.x * inv_w,
            source.y * inv_h,
            source.width * inv_w,
            source.height * inv_h,
        );
        Self {
            source,
            source_uv,
            destination,
        }
    }

    /// Result for a missing or degenerate image: nothing to sample or draw.
    pub fn empty(box_rect: Rect) -> Self {
        Self {
            source: Rect::new(0.0, 0.0, 0.0, 0.0),
            source_uv: Rect::new(0.0, 0.0, 0.0, 0.0),
            destination: Rect::new(box_rect.x, box_rect.y, 0.0, 0.0),
        }
    }
}

/// Fits an image with intrinsic size `intrinsic_width x intrinsic_height` into a
/// replaced-element `box_rect` according to CSS `object-fit` / `object-position`.
///
/// The returned [`FittedImage::source`] is the whole image; the aspect ratio and
/// cropping are expressed entirely through [`FittedImage::destination`]. For
/// `cover` and `none` the destination can be larger than the box, so callers
/// should clip painting to the box (e.g. with a `PushClip` display command).
///
/// `scale-down` is `contain` capped at a scale of `1.0`, so small images are
/// never enlarged.
pub fn fit_image(
    box_rect: Rect,
    intrinsic_width: u32,
    intrinsic_height: u32,
    fit: ObjectFit,
    position: ObjectPosition,
) -> FittedImage {
    if box_rect.is_empty() || intrinsic_width == 0 || intrinsic_height == 0 {
        return FittedImage::empty(box_rect);
    }

    let intrinsic_w = intrinsic_width as f32;
    let intrinsic_h = intrinsic_height as f32;
    let source = Rect::new(0.0, 0.0, intrinsic_w, intrinsic_h);

    let scale_x = box_rect.width / intrinsic_w;
    let scale_y = box_rect.height / intrinsic_h;
    let (draw_width, draw_height) = match fit {
        ObjectFit::Fill => (box_rect.width, box_rect.height),
        ObjectFit::Contain => {
            let scale = scale_x.min(scale_y);
            (intrinsic_w * scale, intrinsic_h * scale)
        }
        ObjectFit::Cover => {
            let scale = scale_x.max(scale_y);
            (intrinsic_w * scale, intrinsic_h * scale)
        }
        ObjectFit::None => (intrinsic_w, intrinsic_h),
        ObjectFit::ScaleDown => {
            let scale = scale_x.min(scale_y).min(1.0);
            (intrinsic_w * scale, intrinsic_h * scale)
        }
    };

    let destination = Rect::new(
        box_rect.x + position.x.resolve(box_rect.width - draw_width),
        box_rect.y + position.y.resolve(box_rect.height - draw_height),
        draw_width,
        draw_height,
    );
    FittedImage::new(source, intrinsic_width, intrinsic_height, destination)
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
    fn placeholder_matches_classic_checkerboard() {
        let (width, height) = (37_u32, 25_u32);
        let mut expected = ImageData::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let cell = ((x / 8) + (y / 8)) % 2;
                let shade = if cell == 0 { 0xdd } else { 0xbb };
                expected.set(x, y, shade, shade, shade, 255);
            }
        }
        let actual = ImageData::placeholder(width, height);
        assert_eq!(
            actual, expected,
            "the tile fast path must match the original pattern"
        );
    }

    #[test]
    fn placeholder_caps_oversized_dimensions() {
        let image = ImageData::placeholder(MAX_IMAGE_DIMENSION + 1000, MAX_IMAGE_DIMENSION + 1000);
        assert_eq!(image.width, MAX_IMAGE_DIMENSION);
        assert_eq!(image.height, MAX_IMAGE_DIMENSION);
        assert_eq!(
            image.pixels.len(),
            MAX_IMAGE_DIMENSION as usize * MAX_IMAGE_DIMENSION as usize * 4
        );
        // The clamped surface is fully painted, not left transparent.
        assert_eq!(image.get(0, 0)[3], 255);
        assert_eq!(
            image.get(MAX_IMAGE_DIMENSION - 1, MAX_IMAGE_DIMENSION - 1)[3],
            255
        );
    }

    #[test]
    fn transparent_helper_has_no_opaque_pixels() {
        let image = ImageData::transparent(8, 6);
        assert_eq!((image.width, image.height), (8, 6));
        assert!(image.pixels.iter().all(|channel| *channel == 0));
        assert_eq!(image.get(0, 0), [0, 0, 0, 0]);
        assert_eq!(image.get(7, 5), [0, 0, 0, 0]);

        // The content placeholder stays a visible checkerboard.
        let placeholder = ImageData::placeholder(8, 6);
        assert_eq!(placeholder.get(0, 0)[3], 255);
    }

    #[test]
    fn sampling_is_clamped() {
        let mut image = ImageData::new(2, 2);
        image.set(1, 1, 1, 2, 3, 255);
        assert_eq!(image.sample(0.75, 0.75), [1, 2, 3, 255]);
        assert_eq!(image.sample(2.0, 2.0), [1, 2, 3, 255]);
    }

    #[test]
    fn new_clamps_oversized_dimensions() {
        let image = ImageData::new(MAX_IMAGE_DIMENSION + 1000, 8);
        assert_eq!(image.width, MAX_IMAGE_DIMENSION);
        assert_eq!(image.height, 8);
        assert_eq!(image.pixels.len(), MAX_IMAGE_DIMENSION as usize * 8 * 4);

        let image = ImageData::new(4, u32::MAX);
        assert_eq!(image.height, MAX_IMAGE_DIMENSION);
        assert_eq!(image.pixels.len(), 4 * MAX_IMAGE_DIMENSION as usize * 4);
    }

    #[test]
    fn from_rgba_rejects_oversized_dimensions() {
        let error = ImageData::from_rgba(MAX_IMAGE_DIMENSION + 1, 1, Vec::new())
            .expect_err("oversized image must be rejected");
        assert!(error.message.contains("exceed"), "{}", error.message);

        let error = ImageData::from_rgba(1, MAX_IMAGE_DIMENSION + 1, Vec::new())
            .expect_err("oversized image must be rejected");
        assert!(error.message.contains("exceed"), "{}", error.message);

        // A correctly sized buffer at the cap still succeeds.
        let at_cap = vec![0; MAX_IMAGE_DIMENSION as usize * 4];
        let image = ImageData::from_rgba(MAX_IMAGE_DIMENSION, 1, at_cap).expect("at the cap");
        assert_eq!(image.width, MAX_IMAGE_DIMENSION);
    }

    #[test]
    fn object_fit_fill_stretches_to_the_box() {
        let box_rect = Rect::new(10.0, 20.0, 100.0, 50.0);
        let fit = fit_image(
            box_rect,
            200,
            100,
            ObjectFit::Fill,
            ObjectPosition::default(),
        );
        assert_eq!(fit.source, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(fit.destination, box_rect);
        assert_eq!(fit.source_uv, Rect::new(0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn object_fit_contain_fits_and_centers() {
        let fit = fit_image(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            200,
            100,
            ObjectFit::Contain,
            ObjectPosition::default(),
        );
        // scale = min(100/200, 100/100) = 0.5 -> 100x50, centered vertically.
        assert_eq!(fit.destination, Rect::new(0.0, 25.0, 100.0, 50.0));
    }

    #[test]
    fn object_fit_cover_crops_and_centers() {
        let fit = fit_image(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            200,
            100,
            ObjectFit::Cover,
            ObjectPosition::default(),
        );
        // scale = max(0.5, 1.0) = 1.0 -> 200x100, centered horizontally.
        assert_eq!(fit.destination, Rect::new(-50.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn object_fit_none_uses_intrinsic_size() {
        let fit = fit_image(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            50,
            100,
            ObjectFit::None,
            ObjectPosition::default(),
        );
        assert_eq!(fit.destination, Rect::new(25.0, 0.0, 50.0, 100.0));
    }

    #[test]
    fn object_fit_scale_down_never_enlarges() {
        let fit = fit_image(
            Rect::new(0.0, 0.0, 200.0, 200.0),
            50,
            50,
            ObjectFit::ScaleDown,
            ObjectPosition::default(),
        );
        // contain would scale by 4; scale-down caps at 1.
        assert_eq!(fit.destination, Rect::new(75.0, 75.0, 50.0, 50.0));
    }

    #[test]
    fn object_position_percentages_align_edges() {
        let box_rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        let left_bottom = fit_image(
            box_rect,
            100,
            50,
            ObjectFit::Contain,
            ObjectPosition::from_percentages(0.0, 1.0),
        );
        // 100x50 drawn: no free horizontal space, 50px free vertically.
        assert_eq!(left_bottom.destination, Rect::new(0.0, 50.0, 100.0, 50.0));

        let right_top = fit_image(
            box_rect,
            100,
            50,
            ObjectFit::Contain,
            ObjectPosition::from_percentages(1.0, 0.0),
        );
        assert_eq!(right_top.destination, Rect::new(0.0, 0.0, 100.0, 50.0));
    }

    #[test]
    fn object_position_moves_the_cover_crop() {
        let fit = fit_image(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            200,
            100,
            ObjectFit::Cover,
            ObjectPosition::from_percentages(0.0, 0.5),
        );
        // 200x100 drawn with -100px free horizontally; 0% pins the left edge.
        assert_eq!(fit.destination, Rect::new(0.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn object_position_lengths_and_negative_lengths() {
        let fit = fit_image(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            100,
            50,
            ObjectFit::Contain,
            ObjectPosition::from_lengths(10.0, -10.0),
        );
        // Free space: x = 0, y = 50. Negative y length is measured from the end.
        assert_eq!(fit.destination, Rect::new(10.0, 40.0, 100.0, 50.0));
        assert_eq!(ObjectPosition::default(), ObjectPosition::center());
    }

    #[test]
    fn degenerate_image_fits_to_nothing() {
        let fit = fit_image(
            Rect::new(5.0, 6.0, 100.0, 100.0),
            0,
            0,
            ObjectFit::Cover,
            ObjectPosition::default(),
        );
        assert_eq!(fit.source, Rect::new(0.0, 0.0, 0.0, 0.0));
        assert_eq!(fit.destination, Rect::new(5.0, 6.0, 0.0, 0.0));
    }
}
