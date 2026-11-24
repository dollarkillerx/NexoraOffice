//! Pictures in documents: PNG and JPEG decoded to RGBA within limits.
//!
//! Office files carry pictures as the bytes of an image file. They come
//! from anyone, so decoding refuses images whose header promises more
//! pixels than [`MAX_PIXELS`] before allocating anything for them, and
//! shrinks large ones to at most `max_side` pixels on their longer side,
//! which is all a slide or a sheet on screen needs. Other formats (EMF,
//! WMF, TIFF, SVG ...) are recognised and reported as unsupported, so the
//! caller can draw a frame in their place.

use std::fmt;
use std::io::Cursor;

/// Largest picture decoded at all: 100 megapixels (a 10,000 × 10,000 photo).
pub const MAX_PIXELS: u64 = 100_000_000;

/// The kinds of picture file Office documents hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Tiff,
    Emf,
    Wmf,
    Svg,
    Unknown,
}

/// What the first bytes say a picture is.
pub fn sniff(bytes: &[u8]) -> Kind {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Kind::Png,
        [0xFF, 0xD8, 0xFF, ..] => Kind::Jpeg,
        [b'G', b'I', b'F', b'8', ..] => Kind::Gif,
        [b'B', b'M', ..] => Kind::Bmp,
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => Kind::Tiff,
        [1, 0, 0, 0, _, _, _, _, ..] if bytes.get(40..44) == Some(b" EMF") => Kind::Emf,
        [0xD7, 0xCD, 0xC6, 0x9A, ..] | [1, 0, 9, 0, ..] | [2, 0, 9, 0, ..] => Kind::Wmf,
        _ if looks_like_svg(bytes) => Kind::Svg,
        _ => Kind::Unknown,
    }
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    String::from_utf8_lossy(head).contains("<svg")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A format we do not decode.
    Unsupported(Kind),
    /// More pixels than [`MAX_PIXELS`].
    TooLarge { width: u32, height: u32 },
    /// The file is damaged.
    Broken(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(kind) => write!(f, "{kind:?} pictures are not decoded"),
            Error::TooLarge { width, height } => write!(f, "{width} × {height} is too large"),
            Error::Broken(why) => write!(f, "broken picture: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// Straight (not premultiplied) RGBA, 8 bits a channel, row by row.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl fmt::Debug for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rgba({} × {})", self.width, self.height)
    }
}

impl Rgba {
    /// Whether any pixel is not fully opaque.
    pub fn has_transparency(&self) -> bool {
        self.pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 0xFF)
    }
}

/// Decode a PNG or JPEG, shrunk to fit `max_side` pixels on its longer
/// side.
pub fn decode(bytes: &[u8], max_side: u32) -> Result<Rgba, Error> {
    let image = match sniff(bytes) {
        Kind::Png => decode_png(bytes)?,
        Kind::Jpeg => decode_jpeg(bytes)?,
        kind => return Err(Error::Unsupported(kind)),
    };
    Ok(shrink(image, max_side.max(1)))
}

fn check(width: u32, height: u32) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::Broken("no pixels".into()));
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::TooLarge { width, height });
    }
    Ok(())
}

fn decode_png(bytes: &[u8]) -> Result<Rgba, Error> {
    let broken = |e: png::DecodingError| Error::Broken(e.to_string());
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            // Room for the largest picture we accept, at 16 bits a channel.
            bytes: (MAX_PIXELS * 8) as usize,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(broken)?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check(width, height)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| Error::Broken("too large".into()))?;
    let mut buffer = vec![0; size];
    let frame = reader.next_frame(&mut buffer).map_err(broken)?;
    buffer.truncate(frame.buffer_size());
    let pixels = (width as usize) * (height as usize);
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 0xFF])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|&v| [v, v, v, 0xFF]).collect(),
        png::ColorType::Indexed => return Err(Error::Broken("palette not expanded".into())),
    };
    if rgba.len() != pixels * 4 {
        return Err(Error::Broken("short image data".into()));
    }
    Ok(Rgba {
        width,
        height,
        pixels: rgba,
    })
}

fn decode_jpeg(bytes: &[u8]) -> Result<Rgba, Error> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let broken = |e: zune_jpeg::errors::DecodeErrors| Error::Broken(e.to_string());
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(1 << 16)
        .set_max_height(1 << 16);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(broken)?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::Broken("no header".into()))?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check(width, height)?;
    let pixels = decoder.decode().map_err(broken)?;
    if pixels.len() != (width as usize) * (height as usize) * 4 {
        return Err(Error::Broken("short image data".into()));
    }
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

/// Shrink by averaging boxes of pixels (weighted by alpha, so transparent
/// pixels do not darken edges) until the longer side fits.
pub fn shrink(image: Rgba, max_side: u32) -> Rgba {
    let longer = image.width.max(image.height);
    if longer <= max_side {
        return image;
    }
    let ratio = f64::from(max_side) / f64::from(longer);
    let width = ((f64::from(image.width) * ratio).round() as u32).max(1);
    let height = ((f64::from(image.height) * ratio).round() as u32).max(1);
    let (sw, sh) = (image.width as usize, image.height as usize);
    let mut out = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let y0 = y * sh / height as usize;
        let y1 = ((y + 1) * sh / height as usize).max(y0 + 1).min(sh);
        for x in 0..width as usize {
            let x0 = x * sw / width as usize;
            let x1 = ((x + 1) * sw / width as usize).max(x0 + 1).min(sw);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let row = &image.pixels[(sy * sw + x0) * 4..(sy * sw + x1) * 4];
                for p in row.as_chunks::<4>().0 {
                    let a = u64::from(p[3]);
                    sum[0] += u64::from(p[0]) * a;
                    sum[1] += u64::from(p[1]) * a;
                    sum[2] += u64::from(p[2]) * a;
                    sum[3] += a;
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u64;
            let at = (y * width as usize + x) * 4;
            if let Some(alpha) = std::num::NonZeroU64::new(sum[3]) {
                for c in 0..3 {
                    out[at + c] = ((sum[c] + alpha.get() / 2) / alpha) as u8;
                }
            }
            out[at + 3] = ((sum[3] + count / 2) / count) as u8;
        }
    }
    Rgba {
        width,
        height,
        pixels: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn decodes_png_in_every_colour_type() {
        let rgb = png(2, 1, png::ColorType::Rgb, &[255, 0, 0, 0, 0, 255]);
        assert_eq!(sniff(&rgb), Kind::Png);
        let image = decode(&rgb, 100).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(!image.has_transparency());
        let gray = decode(&png(1, 1, png::ColorType::GrayscaleAlpha, &[9, 128]), 100).unwrap();
        assert_eq!(gray.pixels, [9, 9, 9, 128]);
        assert!(gray.has_transparency());
        let gray = decode(&png(1, 1, png::ColorType::Grayscale, &[200]), 100).unwrap();
        assert_eq!(gray.pixels, [200, 200, 200, 255]);
    }

    #[test]
    fn large_pictures_shrink_to_the_limit() {
        let data: Vec<u8> = (0..400 * 100).flat_map(|_| [10, 20, 30, 255]).collect();
        let image = decode(&png(400, 100, png::ColorType::Rgba, &data), 100).unwrap();
        assert_eq!((image.width, image.height), (100, 25));
        assert_eq!(&image.pixels[..4], &[10, 20, 30, 255]);
        // Transparent pixels do not darken their neighbours.
        let half = shrink(
            Rgba {
                width: 2,
                height: 1,
                pixels: vec![200, 100, 50, 255, 0, 0, 0, 0],
            },
            1,
        );
        assert_eq!(half.pixels, [200, 100, 50, 128]);
    }

    #[test]
    fn refuses_what_it_cannot_or_should_not_decode() {
        assert_eq!(decode(b"", 10), Err(Error::Unsupported(Kind::Unknown)));
        let mut emf = vec![1, 0, 0, 0, 0, 0, 0, 0];
        emf.resize(40, 0);
        emf.extend_from_slice(b" EMF");
        assert_eq!(decode(&emf, 10), Err(Error::Unsupported(Kind::Emf)));
        assert_eq!(sniff(b"<?xml version='1.0'?><svg/>"), Kind::Svg);
        assert_eq!(sniff(b"GIF89a"), Kind::Gif);
        // Truncated files are broken, not a panic.
        let mut cut = png(4, 4, png::ColorType::Rgb, &[7; 48]);
        cut.truncate(cut.len() / 2);
        assert!(matches!(decode(&cut, 10), Err(Error::Broken(_))));
        assert!(matches!(
            decode(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0], 10),
            Err(Error::Broken(_))
        ));
    }

    #[test]
    fn a_header_promising_too_many_pixels_is_refused_before_decoding() {
        // A real header for 20000 × 20000 followed by no image data.
        let mut huge = png(1, 1, png::ColorType::Rgb, &[0, 0, 0]);
        // Width and height are the first fields of IHDR, at offset 16; fix
        // the CRC after them.
        huge[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        huge[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        let crc = crc32(&huge[12..29]);
        huge[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(
            decode(&huge, 10),
            Err(Error::TooLarge {
                width: 20_000,
                height: 20_000
            })
        );
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn decodes_a_jpeg() {
        // An 8 × 8 grey baseline JPEG.
        const GREY: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08,
            0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C,
            0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
            0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30,
            0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34,
            0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
            0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0xFF, 0xC4, 0x00, 0x14,
            0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00,
            0x3F, 0x00, 0x2A, 0x9F, 0xFF, 0xD9,
        ];
        assert_eq!(sniff(GREY), Kind::Jpeg);
        match decode(GREY, 100) {
            Ok(image) => {
                assert_eq!((image.width, image.height), (8, 8));
                assert_eq!(image.pixels.len(), 8 * 8 * 4);
                assert!(image.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0xFF));
            }
            Err(error) => panic!("{error}"),
        }
    }
}
