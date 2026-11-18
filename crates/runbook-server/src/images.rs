//! UP2 (D51): the native image pipeline, run at finalize for verified images.
//!
//! 1. Width and height come from the file header (`imagesize`, plus the
//!    first frame's descriptor for GIF) before any decode; anything over the
//!    dimension cap or the pixel cap ([`Caps`]) is refused there
//!    (`upload.image_too_large`).
//! 2. The whole pipeline's peak memory is estimated from the header, the
//!    decoded layout and the orientation ([`peak_estimate`]) and must fit
//!    [`PEAK_BUDGET`]; then the `image` crate decodes it (PNG, JPEG, GIF and
//!    WebP only) under the same width, height and allocation limits.
//! 3. The EXIF orientation is applied, then the pixels are re-encoded from
//!    the decoded buffer itself (no conversion copy unless the encoder needs
//!    another layout), so no metadata survives (EXIF and GPS, XMP, ICC
//!    profiles, text chunks, comments).
//! 4. A thumbnail with a long edge of at most [`THUMBNAIL_LONG_EDGE`] px is
//!    encoded the same way.
//!
//! Formats (UP2): PNG stays PNG, JPEG
//! stays JPEG (quality [`JPEG_QUALITY`]), WebP stays WebP (lossless; `image`
//! has no lossy WebP encoder), and GIF becomes a PNG of its first frame.
//! Everything here is synchronous and CPU-bound: the host runs it on a
//! blocking thread, one image at a time.
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits,
    codecs::{jpeg::JpegEncoder, webp::WebPEncoder},
    metadata::Orientation,
};
use std::io::Cursor;

/// D51 / PRD §14: the thumbnail's long edge.
pub const THUMBNAIL_LONG_EDGE: u32 = 320;
/// Re-encoded JPEG quality (photos stay photos; see the UP2 decisions).
pub const JPEG_QUALITY: u8 = 90;
/// The largest width × height the pipeline decodes: 4,096 × 4,096 (16.8 MP).
/// With the costliest layout (16-bit RGBA, rotated, or lossless WebP) its
/// estimated peak is under [`PEAK_BUDGET`]. `/v1/meta` advertises it as
/// `limits.uploads.image_max_pixels`, so clients shrink larger images first.
pub const IMAGE_MAX_PIXELS: u64 = 4_096 * 4_096;
/// The most memory one image may take through the whole pipeline (input,
/// decoder, rotation, encoders, outputs): 384 MiB. One image is processed
/// at a time per server; two would still fit the compose 1 GB limit.
pub const PEAK_BUDGET: u64 = 384 * 1_048_576;

/// The pipeline's limits for one image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Largest width or height.
    pub max_dimension: u32,
    /// Largest width × height.
    pub max_pixels: u64,
}

/// What the pipeline made of one image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Processed {
    /// The re-encoded image, with no metadata.
    pub bytes: Vec<u8>,
    /// Its type, which can differ from the input (GIF becomes PNG).
    pub content_type: &'static str,
    /// After the EXIF orientation is applied.
    pub width: u32,
    pub height: u32,
    pub thumbnail: Vec<u8>,
}

/// Width and height from the header alone, without decoding anything.
pub fn header_dimensions(bytes: &[u8]) -> Result<(u32, u32), &'static str> {
    let size = imagesize::blob_size(bytes).map_err(|_| "upload.image_invalid")?;
    let width = u32::try_from(size.width).map_err(|_| "upload.image_too_large")?;
    let height = u32::try_from(size.height).map_err(|_| "upload.image_too_large")?;
    if width == 0 || height == 0 {
        return Err("upload.image_invalid");
    }
    Ok((width, height))
}

/// A GIF's first image descriptor's width and height (the frame the
/// pipeline decodes), which may exceed the logical screen the header
/// declares. Walks the blocks before it without decoding anything.
pub fn gif_first_frame(bytes: &[u8]) -> Option<(u32, u32)> {
    let u16_at = |at: usize| Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]));
    let packed = *bytes.get(10)?;
    let mut at = 13;
    if packed & 0x80 != 0 {
        at += 3 << ((packed & 0x07) + 1);
    }
    loop {
        match *bytes.get(at)? {
            0x2C => return Some((u32::from(u16_at(at + 5)?), u32::from(u16_at(at + 7)?))),
            0x21 => {
                at += 2;
                // Extension sub-blocks, up to the zero-length terminator.
                loop {
                    let len = usize::from(*bytes.get(at)?);
                    at += 1 + len;
                    if len == 0 {
                        break;
                    }
                }
            }
            _ => return None,
        }
    }
}

fn format(content_type: &str) -> Result<ImageFormat, &'static str> {
    Ok(match content_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        // HEIC and SVG never reach finalize's pipeline (D51).
        _ => return Err("upload.type_unsupported"),
    })
}

fn decode_error(error: image::ImageError) -> &'static str {
    match error {
        image::ImageError::Limits(_) => "upload.image_too_large",
        _ => "upload.image_invalid",
    }
}

fn rotates(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// The pipeline's estimated peak for an image of `pixels` decoded at
/// `bytes_per_pixel`, from an input of `input` bytes: the largest of its
/// phases, each counting every buffer alive at once (measured against a
/// counting allocator in `tests/image_memory.rs`).
/// - decode: the output buffer plus the decoder's own (zune-jpeg's
///   component planes, the PNG decoder's inflate and unfilter buffers,
///   measured at about 2.2× the image, the WebP decoder's ARGB buffer, a
///   GIF frame);
/// - rotation: a second full buffer for 90° and 270° turns;
/// - encode: the buffer, an encoder copy (WebP expands to RGBA) and the
///   output, which for noise can exceed the raw size (WebP holds its frame
///   and our copy of it at once; PNG streams rows into the output).
///
/// 16-bit RGB and RGBA PNGs cost the most: at the pixel cap they exceed
/// [`PEAK_BUDGET`] and are refused (`upload.image_too_large`) up to about
/// 9 MP and 11 MP respectively.
pub fn estimated_peak(
    format: ImageFormat,
    pixels: u64,
    bytes_per_pixel: u64,
    rotated: bool,
    input: u64,
) -> u64 {
    let d = bytes_per_pixel;
    let decode = d + match format {
        ImageFormat::Jpeg => d,
        ImageFormat::WebP => 8,
        ImageFormat::Gif => 4,
        _ => 5 * d / 2,
    };
    let rotate = if rotated { 2 * d } else { d };
    let encode = d + match format {
        // Output at most twice the raw bytes (quality 90 on noise).
        ImageFormat::Jpeg => 2 * d,
        // The streamed output: filter bytes and deflate's stored blocks.
        ImageFormat::Png | ImageFormat::Gif => d + 1,
        // An RGBA copy, then the frame and our copy of it.
        _ => 4 + 2 * 5,
    };
    input + pixels * decode.max(rotate).max(encode)
}

/// Header checks, then the decoder (not yet decoding), its orientation and
/// the estimated peak.
fn prepare<'a>(
    bytes: &'a [u8],
    content_type: &str,
    caps: Caps,
) -> Result<(impl ImageDecoder + 'a, ImageFormat, Orientation, u64), &'static str> {
    let format = format(content_type)?;
    let (width, height) = header_dimensions(bytes)?;
    let mut pixels = u64::from(width) * u64::from(height);
    let fits = |w: u32, h: u32| {
        w <= caps.max_dimension
            && h <= caps.max_dimension
            && u64::from(w) * u64::from(h) <= caps.max_pixels
    };
    if !fits(width, height) {
        return Err("upload.image_too_large");
    }
    if format == ImageFormat::Gif {
        let (w, h) = gif_first_frame(bytes).ok_or("upload.image_invalid")?;
        if !fits(w, h) {
            return Err("upload.image_too_large");
        }
        pixels = pixels.max(u64::from(w) * u64::from(h));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(caps.max_dimension);
    limits.max_image_height = Some(caps.max_dimension);
    limits.max_alloc = Some(PEAK_BUDGET);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    // The decoder's own view must agree with the header.
    let (w, h) = decoder.dimensions();
    if !fits(w, h) {
        return Err("upload.image_too_large");
    }
    // A malformed EXIF block only loses the orientation.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let peak = estimated_peak(
        format,
        pixels.max(u64::from(w) * u64::from(h)),
        u64::from(decoder.color_type().bytes_per_pixel()),
        rotates(orientation),
        bytes.len() as u64,
    );
    if peak > PEAK_BUDGET {
        return Err("upload.image_too_large");
    }
    Ok((decoder, format, orientation, peak))
}

/// The estimated peak memory of processing this image, after the header
/// checks; `Err` is the code [`process`] would refuse it with first.
pub fn peak_estimate(bytes: &[u8], content_type: &str, caps: Caps) -> Result<u64, &'static str> {
    prepare(bytes, content_type, caps).map(|(_, _, _, peak)| peak)
}

/// Check, decode, strip and thumbnail one verified image of `content_type`.
/// Errors are stable rejection codes.
pub fn process(bytes: &[u8], content_type: &str, caps: Caps) -> Result<Processed, &'static str> {
    let (decoder, format, orientation, _) = prepare(bytes, content_type, caps)?;
    // GIF, APNG and animated WebP decode their first frame only.
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    image.apply_orientation(orientation);
    let (out_type, encode): (&'static str, Encode) = match format {
        ImageFormat::Jpeg => ("image/jpeg", encode_jpeg),
        ImageFormat::WebP => ("image/webp", encode_webp),
        _ => ("image/png", encode_png),
    };
    let bytes = encode(&image)?;
    let thumbnail = if image.width().max(image.height()) > THUMBNAIL_LONG_EDGE {
        encode(&image.thumbnail(THUMBNAIL_LONG_EDGE, THUMBNAIL_LONG_EDGE))?
    } else {
        bytes.clone()
    };
    Ok(Processed {
        bytes,
        content_type: out_type,
        width: image.width(),
        height: image.height(),
        thumbnail,
    })
}

/// One output format's encoder.
type Encode = fn(&DynamicImage) -> Result<Vec<u8>, &'static str>;

fn encoded(result: image::ImageResult<()>, out: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    result.map(|()| out).map_err(|_| "upload.image_invalid")
}

/// Encode `image` as is when `accepted` takes its layout, else after
/// `convert` (the only copy the encoders make on our side).
fn write<E: ImageEncoder>(
    encoder: E,
    image: &DynamicImage,
    accepted: &[ExtendedColorType],
    convert: fn(&DynamicImage) -> DynamicImage,
) -> image::ImageResult<()> {
    let color: ExtendedColorType = image.color().into();
    if accepted.contains(&color) {
        encoder.write_image(image.as_bytes(), image.width(), image.height(), color)
    } else {
        let converted = convert(image);
        encoder.write_image(
            converted.as_bytes(),
            converted.width(),
            converted.height(),
            converted.color().into(),
        )
    }
}

fn encode_jpeg(image: &DynamicImage) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    let result = write(
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY),
        image,
        &[ExtendedColorType::Rgb8, ExtendedColorType::L8],
        |i| {
            if i.color().has_color() {
                DynamicImage::ImageRgb8(i.to_rgb8())
            } else {
                DynamicImage::ImageLuma8(i.to_luma8())
            }
        },
    );
    encoded(result, out)
}

/// PNG, streamed: rows are filtered and compressed into the output as they
/// are written, so the encoder holds a few rows, not the image (16-bit
/// samples are swapped to big-endian one row at a time).
fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, &'static str> {
    use png::{BitDepth, ColorType as Png};
    let converted;
    let image = match image.color() {
        image::ColorType::L8
        | image::ColorType::La8
        | image::ColorType::Rgb8
        | image::ColorType::Rgba8
        | image::ColorType::L16
        | image::ColorType::La16
        | image::ColorType::Rgb16
        | image::ColorType::Rgba16 => image,
        _ => {
            converted = DynamicImage::ImageRgba8(image.to_rgba8());
            &converted
        }
    };
    let color = image.color();
    let (png_color, depth) = match color {
        image::ColorType::L8 => (Png::Grayscale, BitDepth::Eight),
        image::ColorType::La8 => (Png::GrayscaleAlpha, BitDepth::Eight),
        image::ColorType::Rgb8 => (Png::Rgb, BitDepth::Eight),
        image::ColorType::L16 => (Png::Grayscale, BitDepth::Sixteen),
        image::ColorType::La16 => (Png::GrayscaleAlpha, BitDepth::Sixteen),
        image::ColorType::Rgb16 => (Png::Rgb, BitDepth::Sixteen),
        image::ColorType::Rgba16 => (Png::Rgba, BitDepth::Sixteen),
        _ => (Png::Rgba, BitDepth::Eight),
    };
    let wide = depth == BitDepth::Sixteen;
    let row_bytes = image.width() as usize * usize::from(color.bytes_per_pixel());
    let mut out = Vec::new();
    let result = (|| -> Result<(), png::EncodingError> {
        use std::io::Write;
        let mut encoder = png::Encoder::new(&mut out, image.width(), image.height());
        encoder.set_color(png_color);
        encoder.set_depth(depth);
        encoder.set_compression(png::Compression::Balanced);
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header()?;
        let mut stream = writer.stream_writer_with_size(64 * 1024)?;
        let mut row = vec![0u8; if wide { row_bytes } else { 0 }];
        for line in image.as_bytes().chunks_exact(row_bytes) {
            if wide {
                let (to, _) = row.as_chunks_mut::<2>();
                let (from, _) = line.as_chunks::<2>();
                for (to, from) in to.iter_mut().zip(from) {
                    *to = u16::from_ne_bytes(*from).to_be_bytes();
                }
                stream.write_all(&row)?;
            } else {
                stream.write_all(line)?;
            }
        }
        stream.finish()?;
        writer.finish()
    })();
    result.map(|()| out).map_err(|_| "upload.image_invalid")
}

fn encode_webp(image: &DynamicImage) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    let result = write(
        WebPEncoder::new_lossless(&mut out),
        image,
        &[
            ExtendedColorType::L8,
            ExtendedColorType::La8,
            ExtendedColorType::Rgb8,
            ExtendedColorType::Rgba8,
        ],
        |i| {
            if i.color().has_alpha() {
                DynamicImage::ImageRgba8(i.to_rgba8())
            } else {
                DynamicImage::ImageRgb8(i.to_rgb8())
            }
        },
    );
    encoded(result, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb, Rgba};

    /// A little-endian TIFF block (the EXIF payload) with an orientation
    /// tag and a GPS IFD holding a latitude and longitude.
    fn gps_exif(orientation: u16) -> Vec<u8> {
        let mut t = b"II*\0".to_vec();
        t.extend(8u32.to_le_bytes());
        // IFD0 at 8: two entries (Orientation, GPS IFD pointer).
        t.extend(2u16.to_le_bytes());
        t.extend(0x0112u16.to_le_bytes());
        t.extend(3u16.to_le_bytes());
        t.extend(1u32.to_le_bytes());
        t.extend(u32::from(orientation).to_le_bytes());
        t.extend(0x8825u16.to_le_bytes());
        t.extend(4u16.to_le_bytes());
        t.extend(1u32.to_le_bytes());
        let gps_ifd = 8 + 2 + 2 * 12 + 4;
        t.extend((gps_ifd as u32).to_le_bytes());
        t.extend(0u32.to_le_bytes());
        // GPS IFD: LatitudeRef 'N', Latitude 3 rationals, LongitudeRef 'W',
        // Longitude 3 rationals.
        let values = gps_ifd + 2 + 4 * 12 + 4;
        t.extend(4u16.to_le_bytes());
        for (tag, kind, count, value) in [
            (1u16, 2u16, 2u32, u32::from_le_bytes(*b"N\0\0\0")),
            (2, 5, 3, values as u32),
            (3, 2, 2, u32::from_le_bytes(*b"W\0\0\0")),
            (4, 5, 3, values as u32 + 24),
        ] {
            t.extend(tag.to_le_bytes());
            t.extend(kind.to_le_bytes());
            t.extend(count.to_le_bytes());
            t.extend(value.to_le_bytes());
        }
        t.extend(0u32.to_le_bytes());
        for (num, den) in [
            (37u32, 1u32),
            (25, 1),
            (1932, 100),
            (122, 1),
            (5, 1),
            (1093, 100),
        ] {
            t.extend(num.to_le_bytes());
            t.extend(den.to_le_bytes());
        }
        t
    }
    fn photo(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x * 7) as u8, (y * 5) as u8, 128])
        }))
    }
    /// A JPEG carrying an EXIF block with GPS coordinates.
    pub(crate) fn gps_jpeg(width: u32, height: u32, orientation: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut out, 80);
        encoder.set_exif_metadata(gps_exif(orientation)).unwrap();
        photo(width, height)
            .to_rgb8()
            .write_with_encoder(encoder)
            .unwrap();
        out
    }
    /// JPEG segment markers before the scan data.
    fn jpeg_markers(bytes: &[u8]) -> Vec<u8> {
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        let mut markers = Vec::new();
        let mut at = 2;
        while at + 4 <= bytes.len() && bytes[at] == 0xFF {
            let marker = bytes[at + 1];
            markers.push(marker);
            if marker == 0xDA {
                break;
            }
            let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
            at += 2 + len;
        }
        markers
    }
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
    fn png_chunks(bytes: &[u8]) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut at = 8;
        while at + 8 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            chunks.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned());
            at += 12 + len;
        }
        chunks
    }

    #[test]
    fn gps_exif_is_stripped_from_jpeg_and_orientation_applied() {
        let input = gps_jpeg(40, 20, 6);
        // The fixture really carries EXIF with a GPS IFD.
        assert!(jpeg_markers(&input).contains(&0xE1));
        assert!(contains(&input, b"Exif\0\0"));
        let mut decoder = ImageReader::with_format(Cursor::new(&input), ImageFormat::Jpeg)
            .into_decoder()
            .unwrap();
        assert!(decoder.exif_metadata().unwrap().is_some());

        let out = process(&input, "image/jpeg", CAPS).unwrap();
        assert_eq!(out.content_type, "image/jpeg");
        // Orientation 6 (rotate 90°) is applied before the EXIF goes.
        assert_eq!((out.width, out.height), (20, 40));
        for bytes in [&out.bytes, &out.thumbnail] {
            let markers = jpeg_markers(bytes);
            assert!(
                !markers
                    .iter()
                    .any(|m| (0xE1..=0xEF).contains(m) || *m == 0xFE),
                "no APP1–APP15 or COM segment: {markers:x?}"
            );
            assert!(!contains(bytes, b"Exif"));
            assert!(!contains(bytes, b"N\0\0\0"));
            let mut decoder = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg)
                .into_decoder()
                .unwrap();
            assert_eq!(decoder.exif_metadata().unwrap(), None);
        }
        // Deterministic: the same input gives the same bytes.
        assert_eq!(process(&input, "image/jpeg", CAPS).unwrap(), out);
    }

    #[test]
    fn png_and_webp_metadata_is_stripped() {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_fn(30, 10, |x, _| {
            Rgba([x as u8, 1, 2, 200])
        }));
        let mut png = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut png);
        encoder.set_exif_metadata(gps_exif(1)).unwrap();
        image.write_with_encoder(encoder).unwrap();
        assert!(png_chunks(&png).contains(&"eXIf".to_owned()));
        let out = process(&png, "image/png", CAPS).unwrap();
        assert_eq!(out.content_type, "image/png");
        let chunks = png_chunks(&out.bytes);
        assert!(
            chunks
                .iter()
                .all(|c| ["IHDR", "IDAT", "IEND"].contains(&c.as_str())),
            "only critical chunks remain: {chunks:?}"
        );
        assert_eq!((out.width, out.height), (30, 10));
        // Pixels, alpha included, survive the lossless re-encode.
        assert_eq!(image::load_from_memory(&out.bytes).unwrap(), image);

        let mut webp = Vec::new();
        let mut encoder = WebPEncoder::new_lossless(&mut webp);
        encoder.set_exif_metadata(gps_exif(1)).unwrap();
        image.write_with_encoder(encoder).unwrap();
        assert!(contains(&webp, b"EXIF"));
        let out = process(&webp, "image/webp", CAPS).unwrap();
        assert_eq!(out.content_type, "image/webp");
        assert!(!contains(&out.bytes, b"EXIF") && !contains(&out.bytes, b"Exif"));
        assert!(!contains(&out.bytes, b"N\0\0\0"));
        assert_eq!(image::load_from_memory(&out.bytes).unwrap(), image);
    }

    #[test]
    fn gif_becomes_a_png_of_its_first_frame() {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_fn(12, 8, |x, _| {
            if x < 6 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        }));
        let mut gif = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut gif), ImageFormat::Gif)
            .unwrap();
        let out = process(&gif, "image/gif", CAPS).unwrap();
        assert_eq!(out.content_type, "image/png");
        assert!(out.bytes.starts_with(b"\x89PNG"));
        assert_eq!(
            image::load_from_memory(&out.bytes).unwrap().to_rgba8(),
            image.to_rgba8()
        );
    }

    #[test]
    fn thumbnails_have_a_320_px_long_edge_and_never_upscale() {
        let wide = process(&gps_jpeg(1000, 500, 1), "image/jpeg", CAPS).unwrap();
        let thumb = image::load_from_memory(&wide.thumbnail).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (320, 160));
        let mut tall = Vec::new();
        photo(300, 900)
            .write_to(&mut Cursor::new(&mut tall), ImageFormat::Png)
            .unwrap();
        let tall = process(&tall, "image/png", CAPS).unwrap();
        let thumb = image::load_from_memory(&tall.thumbnail).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (107, 320));
        let small = process(&gps_jpeg(100, 50, 1), "image/jpeg", CAPS).unwrap();
        let thumb = image::load_from_memory(&small.thumbnail).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (100, 50));
        assert_eq!(small.thumbnail, small.bytes);
    }

    /// A PNG whose header claims `width` × `height` and whose image data is
    /// garbage: refusing it as too large proves the header decided, since
    /// decoding would fail as invalid.
    fn header_only_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend(width.to_be_bytes());
        ihdr.extend(height.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        png.extend(13u32.to_be_bytes());
        png.extend(&ihdr);
        png.extend(crc32(&ihdr).to_be_bytes());
        png.extend(4u32.to_be_bytes());
        png.extend(b"IDATjunk\0\0\0\0");
        png
    }
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn the_pixel_cap_is_checked_from_the_header_at_the_cap_and_one_over() {
        let at = |w, h| process(&header_only_png(w, h), "image/png", CAPS);
        // At the cap the header passes (the junk body then fails to decode).
        assert_eq!(at(4_096, 4_096), Err("upload.image_invalid"));
        assert_eq!(at(8_000, 2_097), Err("upload.image_invalid"));
        // One row or column over is refused before any decoder exists.
        assert_eq!(at(4_097, 4_096), Err("upload.image_too_large"));
        assert_eq!(at(8_000, 2_098), Err("upload.image_too_large"));
        assert_eq!(
            peak_estimate(&header_only_png(4_097, 4_096), "image/png", CAPS),
            Err("upload.image_too_large")
        );
    }

    #[test]
    fn the_worst_case_at_the_cap_fits_the_budget() {
        // Every decoded layout the four decoders produce, rotated or not,
        // at the pixel cap, with the largest input the server accepts.
        let input = 64 * 1_048_576;
        let peak = |content_type, bytes_per_pixel, rotates| {
            estimated_peak(
                format(content_type).unwrap(),
                IMAGE_MAX_PIXELS,
                bytes_per_pixel,
                rotates,
                input,
            )
        };
        for (content_type, bytes_per_pixel) in [
            ("image/jpeg", 1),
            ("image/jpeg", 3),
            ("image/png", 1),
            ("image/png", 2),
            ("image/png", 3),
            ("image/png", 4),
            ("image/gif", 4),
            ("image/webp", 3),
            ("image/webp", 4),
        ] {
            for rotates in [false, true] {
                let peak = peak(content_type, bytes_per_pixel, rotates);
                assert!(
                    peak <= PEAK_BUDGET,
                    "{content_type} {bytes_per_pixel} B/px rotated={rotates}: {peak} > {PEAK_BUDGET}"
                );
            }
        }
        // 16-bit RGB and RGBA PNGs do not fit at the cap: the estimate
        // refuses them there.
        assert!(peak("image/png", 6, false) > PEAK_BUDGET);
        assert!(peak("image/png", 8, false) > PEAK_BUDGET);
        // Two finalizes at once would still fit the compose 1 GB limit.
        const { assert!(2 * PEAK_BUDGET < 1_000_000_000) };
    }

    /// A GIF whose 10 × 10 screen hides a first frame of `w` × `h`.
    fn gif_with_frame(w: u16, h: u16) -> Vec<u8> {
        let mut gif = b"GIF89a".to_vec();
        gif.extend(10u16.to_le_bytes());
        gif.extend(10u16.to_le_bytes());
        gif.extend([0, 0, 0]);
        // A comment extension before the frame is skipped.
        gif.extend([0x21, 0xFE, 3, b'h', b'i', b'!', 0]);
        gif.push(0x2C);
        gif.extend([0, 0, 0, 0]);
        gif.extend(w.to_le_bytes());
        gif.extend(h.to_le_bytes());
        gif.extend([0, 2, 1, 0, 0, 0x3B]);
        gif
    }

    #[test]
    fn a_gif_frame_beyond_the_caps_is_refused_before_decoding() {
        assert_eq!(
            gif_first_frame(&gif_with_frame(5_000, 5_000)),
            Some((5_000, 5_000))
        );
        assert_eq!(
            process(&gif_with_frame(5_000, 5_000), "image/gif", CAPS),
            Err("upload.image_too_large")
        );
        assert_eq!(
            process(&gif_with_frame(9_000, 1), "image/gif", CAPS),
            Err("upload.image_too_large")
        );
        assert_eq!(
            process(&gif_with_frame(8, 8), "image/gif", CAPS),
            Err("upload.image_invalid")
        );
    }

    #[test]
    fn dimensions_are_refused_from_the_header_before_any_decode() {
        assert_eq!(
            header_dimensions(&header_only_png(9_000, 10)),
            Ok((9_000, 10))
        );
        assert_eq!(
            process(&header_only_png(9_000, 10), "image/png", CAPS),
            Err("upload.image_too_large")
        );
        assert_eq!(
            process(&header_only_png(10, 8_001), "image/png", CAPS),
            Err("upload.image_too_large")
        );
        // A lower configured cap applies the same way.
        assert_eq!(
            process(&header_only_png(500, 10), "image/png", caps(400)),
            Err("upload.image_too_large")
        );
        // Within the cap, the garbage body fails at decode.
        assert_eq!(
            process(&header_only_png(500, 10), "image/png", CAPS),
            Err("upload.image_invalid")
        );
        // A JPEG's SOF header is read the same way.
        let jpeg = gps_jpeg(64, 16, 1);
        assert_eq!(header_dimensions(&jpeg), Ok((64, 16)));
        assert_eq!(
            process(&jpeg, "image/jpeg", caps(32)),
            Err("upload.image_too_large")
        );
        // No header at all.
        assert_eq!(
            process(b"\x89PNG\r\n\x1a\n", "image/png", CAPS),
            Err("upload.image_invalid")
        );
        assert_eq!(
            process(&jpeg, "image/heic", CAPS),
            Err("upload.type_unsupported")
        );
    }

    const CAPS: Caps = Caps {
        max_dimension: 8_000,
        max_pixels: IMAGE_MAX_PIXELS,
    };
    fn caps(max_dimension: u32) -> Caps {
        Caps {
            max_dimension,
            max_pixels: IMAGE_MAX_PIXELS,
        }
    }

    #[test]
    fn the_peak_estimate_stops_bombs_the_caps_allow() {
        // With the pixel cap lifted, 8,000 × 8,000 at 16 bits per RGBA
        // channel (512 MB decoded, twice that while encoding) passes the
        // header checks; the peak estimate refuses it before any decode.
        let mut png = header_only_png(8_000, 8_000);
        png[24] = 16;
        let ihdr_crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&ihdr_crc.to_be_bytes());
        let lifted = Caps {
            max_dimension: 8_000,
            max_pixels: 64_000_000,
        };
        assert_eq!(
            peak_estimate(&png, "image/png", lifted),
            Err("upload.image_too_large")
        );
        assert_eq!(
            process(&png, "image/png", lifted),
            Err("upload.image_too_large")
        );
    }
}
