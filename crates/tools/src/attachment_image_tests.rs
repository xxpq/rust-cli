//! 用户附上的图片的缩放（docs/specs/rust-cli/session-lifecycle.md §10.41 引的规则表）。

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use rust_cli_base::proto::mime;
use rust_cli_kernel_api::image::MAX_IMAGE_BYTES;

use super::*;

fn encoded(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}

/// 一张纯色图。
fn solid(width: u32, height: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([10, 120, 200])))
}

/// 压不小的噪点图（固定种子的线性同余）。
fn noise(width: u32, height: u32) -> DynamicImage {
    let mut state: u32 = 0x1234_5678;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 24) as u8
    };
    DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |_, _| {
        Rgb([next(), next(), next()])
    }))
}

fn dimensions(prepared: &Prepared) -> (u32, u32) {
    let image = image::load_from_memory(&prepared.bytes).unwrap();
    (image.width(), image.height())
}

/// 尺寸与字节都在上限内：原样（字节不变）。
#[test]
fn a_small_image_is_kept_as_is() {
    let bytes = encoded(&solid(64, 48), ImageFormat::Png);
    let prepared = prepare(bytes.clone()).unwrap();
    assert_eq!(
        prepared,
        Prepared {
            media_type: mime::PNG,
            bytes,
            changed: false,
        }
    );
}

/// 超过 2000 像素的 PNG：缩到最长边 2000（Jimp 的取整），PNG 只有原尺寸的一次机会，之后转 JPEG（质量 80）。
#[test]
fn an_oversized_png_becomes_a_bounded_jpeg() {
    let prepared = prepare(encoded(&solid(2400, 10), ImageFormat::Png)).unwrap();
    assert_eq!(prepared.media_type, mime::JPEG);
    assert!(prepared.changed);
    // round(10 × 2000 / 2400) = 8
    assert_eq!(dimensions(&prepared), (2000, 8));
}

/// 尺寸在上限内、字节超了的 PNG：先试最高压缩的 PNG（噪点压不小），再转 JPEG，尺寸不变。
#[test]
fn a_heavy_png_within_the_dimension_becomes_a_jpeg_of_the_same_size() {
    let bytes = encoded(&noise(1200, 1200), ImageFormat::Png);
    assert!(bytes.len() > MAX_IMAGE_BYTES, "{}", bytes.len());
    let prepared = prepare(bytes).unwrap();
    assert_eq!(prepared.media_type, mime::JPEG);
    assert!(prepared.bytes.len() <= MAX_IMAGE_BYTES);
    assert_eq!(dimensions(&prepared), (1200, 1200));
}

/// 超尺寸的 JPEG 与 GIF 保持格式（源不是 PNG）：缩到 2000 之后按源格式编。
#[test]
fn an_oversized_jpeg_or_gif_keeps_its_format() {
    for (format, media_type) in [
        (ImageFormat::Jpeg, mime::JPEG),
        (ImageFormat::Gif, mime::GIF),
    ] {
        let image = if format == ImageFormat::Gif {
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(2200, 1100, Rgba([1, 2, 3, 255])))
        } else {
            solid(2200, 1100)
        };
        let prepared = prepare(encoded(&image, format)).unwrap();
        assert_eq!(prepared.media_type, media_type, "{format:?}");
        assert_eq!(dimensions(&prepared), (2000, 1000), "{format:?}");
    }
}

/// WebP 不转码：在预算内原样，超了是 `WebpTooLarge`（只看文件头，不解码）。
#[test]
fn webp_passes_through_or_fails() {
    let header = |size: usize| {
        let mut bytes = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        bytes.resize(size, 0);
        bytes
    };
    let small = header(64);
    assert_eq!(
        prepare(small.clone()).unwrap(),
        Prepared {
            media_type: mime::WEBP,
            bytes: small,
            changed: false,
        }
    );
    assert_eq!(
        prepare(header(MAX_IMAGE_BYTES + 1)),
        Err(PrepareError::WebpTooLarge)
    );
}

/// BMP：在预算内原样发（`image/bmp`，Node 的行为，E2E T17）；超尺寸的按 PNG 的候选转成 JPEG。
#[test]
fn bmp_is_kept_or_converted_like_png() {
    let small = encoded(&solid(4, 4), ImageFormat::Bmp);
    let kept = prepare(small.clone()).unwrap();
    assert_eq!(
        kept,
        Prepared {
            media_type: mime::BMP,
            bytes: small,
            changed: false,
        }
    );
    let converted = prepare(encoded(&solid(2100, 20), ImageFormat::Bmp)).unwrap();
    assert_eq!(converted.media_type, mime::JPEG);
    assert_eq!(dimensions(&converted), (2000, 19));
}

/// 解不开的：空的是 `Empty`；认不出的、文件头对但内容坏的是 `Undecodable`（调用方换成占位文字）。
#[test]
fn bad_bytes_are_refused() {
    assert_eq!(prepare(Vec::new()), Err(PrepareError::Empty));
    assert_eq!(prepare(vec![0; 64]), Err(PrepareError::Undecodable));
    let mut broken = encoded(&solid(8, 8), ImageFormat::Png);
    broken.truncate(20);
    assert_eq!(prepare(broken), Err(PrepareError::Undecodable));
}
