//! UP2 fixes: the image pipeline's real peak memory stays within its own
//! estimate (`images::peak_estimate`), which is what the server checks
//! against `images::PEAK_BUDGET` before decoding. A counting allocator
//! measures every allocation; this file holds one test, so nothing else
//! allocates while it measures. Noise images are the worst case for every
//! encoder's output size; EXIF orientation 6 forces the rotation copy.
use image::{
    DynamicImage, ImageBuffer, ImageEncoder, Rgb, Rgba,
    codecs::{gif::GifEncoder, jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
};
use runbook_server::images::{self, Caps};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Counting;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
        PEAK.fetch_max(now, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            let now = CURRENT.fetch_add(new_size - layout.size(), Ordering::SeqCst) + new_size
                - layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        } else {
            CURRENT.fetch_sub(layout.size() - new_size, Ordering::SeqCst);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes allocated at the peak of `f`, above what was live before it.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CURRENT.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let out = f();
    (out, PEAK.load(Ordering::SeqCst) - base)
}

/// An EXIF block with only an orientation tag.
fn orientation_exif(orientation: u16) -> Vec<u8> {
    let mut t = b"II*\0".to_vec();
    t.extend(8u32.to_le_bytes());
    t.extend(1u16.to_le_bytes());
    t.extend(0x0112u16.to_le_bytes());
    t.extend(3u16.to_le_bytes());
    t.extend(1u32.to_le_bytes());
    t.extend(u32::from(orientation).to_le_bytes());
    t.extend(0u32.to_le_bytes());
    t
}
/// xorshift noise: incompressible pixels.
fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

const W: u32 = 1200;
const H: u32 = 900;

#[test]
fn the_pipeline_peak_stays_within_its_estimate() {
    let px = (W * H) as usize;
    let rgb = ImageBuffer::<Rgb<u8>, _>::from_raw(W, H, noise(px * 3)).unwrap();
    let rgba = ImageBuffer::<Rgba<u8>, _>::from_raw(W, H, noise(px * 4)).unwrap();
    let rgba16 = ImageBuffer::<Rgba<u16>, _>::from_raw(
        W,
        H,
        noise(px * 8)
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect(),
    )
    .unwrap();
    let mut cases: Vec<(&str, &str, Vec<u8>)> = Vec::new();
    let mut jpeg = Vec::new();
    let mut e = JpegEncoder::new_with_quality(&mut jpeg, 100);
    e.set_exif_metadata(orientation_exif(6)).unwrap();
    rgb.write_with_encoder(e).unwrap();
    cases.push(("jpeg rotated", "image/jpeg", jpeg));
    for (name, image, exif) in [
        ("png rgba8", DynamicImage::ImageRgba8(rgba.clone()), None),
        (
            "png rgba16 rotated",
            DynamicImage::ImageRgba16(rgba16),
            Some(6),
        ),
        (
            "png rgb8 rotated",
            DynamicImage::ImageRgb8(rgb.clone()),
            Some(6),
        ),
    ] {
        let mut png = Vec::new();
        let mut e = PngEncoder::new(&mut png);
        if let Some(o) = exif {
            e.set_exif_metadata(orientation_exif(o)).unwrap();
        }
        image.write_with_encoder(e).unwrap();
        cases.push((name, "image/png", png));
    }
    for (name, image) in [
        ("webp rgba rotated", DynamicImage::ImageRgba8(rgba.clone())),
        ("webp rgb rotated", DynamicImage::ImageRgb8(rgb.clone())),
    ] {
        let mut webp = Vec::new();
        let mut e = WebPEncoder::new_lossless(&mut webp);
        e.set_exif_metadata(orientation_exif(6)).unwrap();
        image.write_with_encoder(e).unwrap();
        cases.push((name, "image/webp", webp));
    }
    let mut gif = Vec::new();
    // A 256-colour noise frame, encoded as it comes (no quantizing).
    let paletted = ImageBuffer::<Rgba<u8>, _>::from_fn(W, H, |x, y| {
        let v = ((x * 7 + y * 13) % 256) as u8;
        Rgba([v, v.wrapping_mul(3), v.wrapping_mul(5), 255])
    });
    GifEncoder::new_with_speed(&mut gif, 30)
        .encode(paletted.as_raw(), W, H, image::ExtendedColorType::Rgba8)
        .unwrap();
    cases.push(("gif", "image/gif", gif));

    let caps = Caps {
        max_dimension: 8_000,
        max_pixels: images::IMAGE_MAX_PIXELS,
    };
    for (name, content_type, bytes) in &cases {
        let estimate = images::peak_estimate(bytes, content_type, caps).unwrap();
        let (out, peak) = peak_of(|| images::process(bytes, content_type, caps));
        let out = out.unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(out.width * out.height == W * H, "{name}");
        // The estimate counts the input, which was allocated before.
        let budget = estimate - bytes.len() as u64;
        assert!(
            peak as u64 <= budget,
            "{name}: measured peak {peak} > estimate {budget}"
        );
        eprintln!(
            "{name}: peak {:.1} B/px, estimate {:.1} B/px",
            peak as f64 / f64::from(W * H),
            budget as f64 / f64::from(W * H)
        );
    }
}
