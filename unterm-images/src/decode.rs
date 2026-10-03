//! Turning what a program sent into pixels.
//!
//! Every path ends in the same place -- straight RGBA, eight bits a channel,
//! rows top to bottom -- because that is what a front end uploads as a
//! texture. Every path is also bounded: a program can claim any size it
//! likes, and a terminal that believes a header asking for 100,000 by
//! 100,000 pixels has handed it 40 GB of somebody else's memory.

use std::io::{Cursor, Read};

/// The most pixels one image may have: a little over 8K by 4K.
///
/// Enough for a full-resolution screenshot of any display a terminal runs
/// on, and 160 MB once decoded -- which is already more than a pane should
/// spend on one picture.
pub const MAX_PIXELS: u64 = 40_000_000;

/// The most compressed or encoded bytes accepted for one image.
pub const MAX_ENCODED_BYTES: usize = 100 * 1024 * 1024;

/// An image as a front end draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes of straight-alpha RGBA.
    pub rgba: Vec<u8>,
}

impl Decoded {
    pub fn bytes(&self) -> usize {
        self.rgba.len()
    }
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("the image has no pixels".to_string());
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!(
            "{width}x{height} is larger than the {MAX_PIXELS} pixels one image may have"
        ));
    }
    Ok(())
}

/// A picture file: PNG, JPEG, GIF (its first frame), WebP or BMP -- what
/// terminals are actually sent.
///
/// Each format's decoder is called by name rather than through the crate's
/// generic reader, which would link every format it knows (EXR, TIFF, HDR and
/// the rest) into a kernel that will never see one.
pub fn encoded(bytes: &[u8]) -> Result<Decoded, String> {
    use image::codecs::{bmp::BmpDecoder, gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder};
    use image::{DynamicImage, ImageDecoder, ImageFormat};

    if bytes.len() > MAX_ENCODED_BYTES {
        return Err("the image file is too large".to_string());
    }

    fn decode<D: ImageDecoder>(mut decoder: D) -> Result<Decoded, String> {
        // The header first: a size claim is checked before anything is
        // allocated for it.
        let (width, height) = decoder.dimensions();
        check_size(width, height)?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(width);
        limits.max_image_height = Some(height);
        limits.max_alloc = Some(MAX_PIXELS * 4 * 2);
        decoder
            .set_limits(limits)
            .map_err(|err| format!("could not decode the image: {err}"))?;
        let rgba = DynamicImage::from_decoder(decoder)
            .map_err(|err| format!("could not decode the image: {err}"))?
            .to_rgba8();
        Ok(Decoded {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        })
    }

    let error = |err: image::ImageError| format!("could not read the image: {err}");
    let cursor = Cursor::new(bytes);
    match image::guess_format(bytes) {
        Ok(ImageFormat::Png) => decode(PngDecoder::new(cursor).map_err(error)?),
        Ok(ImageFormat::Jpeg) => decode(JpegDecoder::new(cursor).map_err(error)?),
        Ok(ImageFormat::Gif) => decode(GifDecoder::new(cursor).map_err(error)?),
        Ok(ImageFormat::WebP) => decode(WebPDecoder::new(cursor).map_err(error)?),
        Ok(ImageFormat::Bmp) => decode(BmpDecoder::new(cursor).map_err(error)?),
        _ => Err("not a picture format Unterm reads (PNG, JPEG, GIF, WebP, BMP)".to_string()),
    }
}

/// Raw pixels: three bytes a pixel (RGB) or four (RGBA), as the kitty
/// protocol's `f=24` and `f=32` send them.
pub fn raw(bytes: &[u8], width: u32, height: u32, channels: usize) -> Result<Decoded, String> {
    check_size(width, height)?;
    let pixels = width as usize * height as usize;
    let expected = pixels * channels;
    if bytes.len() < expected {
        return Err(format!(
            "{width}x{height} needs {expected} bytes of pixel data, but {} arrived",
            bytes.len()
        ));
    }
    let rgba = match channels {
        4 => bytes[..expected].to_vec(),
        3 => {
            let mut out = Vec::with_capacity(pixels * 4);
            for pixel in bytes[..expected].chunks_exact(3) {
                out.extend_from_slice(pixel);
                out.push(255);
            }
            out
        }
        _ => return Err(format!("{channels} bytes a pixel is not a format")),
    };
    Ok(Decoded {
        width,
        height,
        rgba,
    })
}

/// The pixels, scaled down to fit `side` by `side` if they do not already.
pub fn fit_within(decoded: &Decoded, side: u32) -> (u32, u32, Vec<u8>) {
    let (width, height) = (decoded.width, decoded.height);
    if width <= side && height <= side {
        return (width, height, decoded.rgba.clone());
    }
    let scale = f64::from(side) / f64::from(width.max(height));
    let fit_w = ((f64::from(width) * scale).round() as u32).clamp(1, side);
    let fit_h = ((f64::from(height) * scale).round() as u32).clamp(1, side);
    let source = image::RgbaImage::from_raw(width, height, decoded.rgba.clone())
        .expect("a decoded picture holds width * height * 4 bytes");
    let scaled = image::imageops::resize(&source, fit_w, fit_h, image::imageops::FilterType::Triangle);
    (fit_w, fit_h, scaled.into_raw())
}

/// zlib (RFC 1950), the kitty protocol's `o=z`.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let limit = (MAX_PIXELS * 4) as u64 + 1;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|err| format!("could not decompress the data: {err}"))?;
    if out.len() as u64 >= limit {
        return Err("the decompressed data is too large".to_string());
    }
    Ok(out)
}

/// A stable name for the pixels, so the same picture sent twice is stored
/// once and a front end can cache it by name.
pub fn content_key(decoded: &Decoded) -> String {
    // FNV-1a over the pixels and the size: cheap, and stable across runs
    // and processes, which a front end's cache relies on.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for byte in decoded.width.to_le_bytes() {
        eat(byte);
    }
    for byte in decoded.height.to_le_bytes() {
        eat(byte);
    }
    for &byte in &decoded.rgba {
        eat(byte);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
pub(crate) fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut out),
        rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("encode a png");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_comes_back_as_its_pixels() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 128];
        let decoded = encoded(&png(2, 1, &pixels)).unwrap();

        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.rgba, pixels);
    }

    #[test]
    fn something_that_is_not_an_image_is_refused() {
        assert!(encoded(b"hello, this is text").is_err());
    }

    #[test]
    fn raw_rgb_gains_an_opaque_alpha() {
        let decoded = raw(&[1, 2, 3, 4, 5, 6], 2, 1, 3).unwrap();
        assert_eq!(decoded.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn raw_pixels_that_fall_short_are_refused() {
        assert!(raw(&[1, 2, 3], 2, 1, 4).is_err());
    }

    #[test]
    fn a_size_claim_beyond_the_limit_is_refused_before_allocating() {
        let err = raw(&[], 100_000, 100_000, 4).unwrap_err();
        assert!(err.contains("larger"), "{err}");
    }

    #[test]
    fn a_large_picture_is_scaled_to_fit_and_a_small_one_is_not() {
        let big = raw(&vec![7u8; 400 * 100 * 4], 400, 100, 4).unwrap();
        let (w, h, rgba) = fit_within(&big, 100);
        assert_eq!((w, h), (100, 25));
        assert_eq!(rgba.len(), 100 * 25 * 4);
        assert_eq!(&rgba[..4], &[7, 7, 7, 7]);

        let small = raw(&[1, 2, 3, 4], 1, 1, 4).unwrap();
        assert_eq!(fit_within(&small, 100), (1, 1, vec![1, 2, 3, 4]));
    }

    #[test]
    fn zlib_round_trips() {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"pixels pixels pixels").unwrap();
        let compressed = encoder.finish().unwrap();

        assert_eq!(inflate(&compressed).unwrap(), b"pixels pixels pixels");
        assert!(inflate(b"not zlib").is_err());
    }

    #[test]
    fn the_same_pixels_get_the_same_key_and_different_ones_do_not() {
        let a = raw(&[1, 2, 3, 4], 1, 1, 4).unwrap();
        let b = raw(&[1, 2, 3, 4], 1, 1, 4).unwrap();
        let c = raw(&[1, 2, 3, 5], 1, 1, 4).unwrap();

        assert_eq!(content_key(&a), content_key(&b));
        assert_ne!(content_key(&a), content_key(&c));
    }
}
