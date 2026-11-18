//! Photo processing (resize, fix orientation, strip metadata) and reading
//! barcodes from photos.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use rand::Rng;

pub const MAX_UPLOAD: usize = 20 << 20;
const FULL_EDGE: u32 = 1600;
const THUMB_EDGE: u32 = 360;

/// A saved photo: `<name>.jpg` plus a `<name>.t.jpg` thumbnail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub file: String,
    pub width: u32,
    pub height: u32,
}

pub fn thumb_name(file: &str) -> String {
    match file.strip_suffix(".jpg") {
        Some(stem) => format!("{stem}.t.jpg"),
        None => format!("{file}.t.jpg"),
    }
}

/// Is `name` a file name we generated (so it is safe to serve or delete)?
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !name.starts_with('.')
        && !name.contains("..")
}

fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    if bytes.len() > MAX_UPLOAD {
        bail!("That photo is larger than 20 MB.");
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("reading photo")?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| anyhow::anyhow!("That file isn't a photo we can read (use JPEG, PNG, WebP or GIF)."))?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).context("decoding photo")?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Decodes, orients, resizes and re-encodes a photo into `dir`. Re-encoding
/// drops EXIF data (including any GPS position) from the stored copy.
pub fn save_photo(dir: &Path, bytes: &[u8]) -> Result<Saved> {
    let img = decode(bytes)?;
    std::fs::create_dir_all(dir)?;
    let stem: String = {
        let mut rng = rand::thread_rng();
        (0..20).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect()
    };
    let file = format!("{stem}.jpg");
    let full = shrink(&img, FULL_EDGE);
    let thumb = shrink(&img, THUMB_EDGE);
    write_jpeg(&dir.join(&file), &full, 84)?;
    write_jpeg(&dir.join(thumb_name(&file)), &thumb, 80)?;
    Ok(Saved { file, width: full.width(), height: full.height() })
}

fn shrink(img: &DynamicImage, edge: u32) -> DynamicImage {
    if img.width() <= edge && img.height() <= edge {
        img.clone()
    } else {
        img.resize(edge, edge, FilterType::Lanczos3)
    }
}

fn write_jpeg(path: &Path, img: &DynamicImage, quality: u8) -> Result<()> {
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality).encode_image(&rgb)?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Removes a photo and its thumbnail (missing files are fine).
pub fn delete_photo(dir: &Path, file: &str) {
    if !is_safe_name(file) {
        return;
    }
    for f in [file.to_owned(), thumb_name(file)] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

/// Reads a barcode (EAN-13/8, UPC-A/E, ISBN) from a photo.
pub fn read_barcode(bytes: &[u8]) -> Result<String> {
    use rxing::{BarcodeFormat, DecodeHints};
    let img = decode(bytes)?;
    // Barcode readers work best around 1000–1600 px.
    let img = shrink(&img, 1600);
    let attempts = [img.clone(), img.rotate90()];
    for candidate in attempts {
        let luma = candidate.to_luma8();
        let (w, h) = luma.dimensions();
        let mut hints = DecodeHints { TryHarder: Some(true), ..Default::default() };
        if let Ok(results) = rxing::helpers::detect_multiple_in_luma_with_hints(luma.into_raw(), w, h, &mut hints) {
            let mut best: Option<String> = None;
            for r in results {
                let text = r.getText().trim().to_owned();
                let linear = matches!(
                    r.getBarcodeFormat(),
                    BarcodeFormat::EAN_13 | BarcodeFormat::EAN_8 | BarcodeFormat::UPC_A | BarcodeFormat::UPC_E
                );
                if linear {
                    return Ok(text);
                }
                best.get_or_insert(text);
            }
            if let Some(b) = best {
                return Ok(b);
            }
        }
    }
    bail!("No barcode found. Try again with the barcode filling more of the photo, or type the number.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 255) as u8, (y % 255) as u8, 90]));
        let mut buf = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn saves_resized_photo_and_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let saved = save_photo(dir.path(), &png(3200, 1600)).unwrap();
        assert_eq!((saved.width, saved.height), (1600, 800));
        assert!(is_safe_name(&saved.file));
        let thumb = image::open(dir.path().join(thumb_name(&saved.file))).unwrap();
        assert_eq!(thumb.width(), 360);
        delete_photo(dir.path(), &saved.file);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn small_photos_are_not_enlarged() {
        let dir = tempfile::tempdir().unwrap();
        let saved = save_photo(dir.path(), &png(200, 100)).unwrap();
        assert_eq!((saved.width, saved.height), (200, 100));
    }

    #[test]
    fn rejects_non_images() {
        let dir = tempfile::tempdir().unwrap();
        let err = save_photo(dir.path(), b"definitely not a picture").unwrap_err();
        assert!(err.to_string().contains("isn't a photo"));
    }

    #[test]
    fn safe_names() {
        assert!(is_safe_name("abc123.jpg"));
        assert!(is_safe_name("abc123.t.jpg"));
        assert!(!is_safe_name("../etc/passwd"));
        assert!(!is_safe_name(".hidden"));
        assert!(!is_safe_name("a/b.jpg"));
        assert!(!is_safe_name(""));
        assert_eq!(thumb_name("x.jpg"), "x.t.jpg");
    }

    #[test]
    fn reads_a_generated_ean13() {
        use rxing::{BarcodeFormat, Writer, oned::EAN13Writer};
        let matrix = EAN13Writer.encode("9780261102958", &BarcodeFormat::EAN_13, 600, 200).unwrap();
        let (w, h) = (matrix.getWidth(), matrix.getHeight());
        // Add a white quiet zone around the code, as on a real book.
        let img = image::GrayImage::from_fn(w + 80, h + 80, |x, y| {
            let inside = x >= 40 && y >= 40 && x < w + 40 && y < h + 40;
            let black = inside && matrix.get(x - 40, y - 40);
            image::Luma([if black { 0 } else { 255 }])
        });
        let mut buf = Vec::new();
        DynamicImage::ImageLuma8(img).write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png).unwrap();
        assert_eq!(read_barcode(&buf).unwrap(), "9780261102958");
        assert!(read_barcode(&png(300, 300)).is_err());
    }
}
