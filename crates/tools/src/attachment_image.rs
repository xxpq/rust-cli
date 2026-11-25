//! 用户附上的图片发给模型之前的缩放与转码（docs/specs/rust-cli/session-lifecycle.md §10.41：规则与 Node 的出处）。
//!
//! 照 Node 的 `prepareForModel`：最长边不超过 2000 像素、字节不超过 [`MAX_IMAGE_BYTES`]（base64 之后正好 5 MiB）。尺寸与字节都在
//! 上限内的原样；否则按 Node 的候选顺序（保持格式 → 缩到 2000 → JPEG 质量 80…20 → 再缩 → 最后的小图）取第一个在预算内的。
//! 只在用户附上的图片上用（前端上传的与本机路径的）；Read 与 TUI 的粘贴不经过这里（tools-files.md D4）。
//!
//! 这是 CPU 工作：调用方放进阻塞线程池（`rust_cli_base::blocking`）。

use std::io::Cursor;

use image::codecs::gif::GifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, Frame, ImageDecoder, ImageFormat, ImageReader};
use rust_cli_base::proto::mime;
use rust_cli_kernel_api::image::MAX_IMAGE_BYTES;

/// 最长边的上限（Node `MAX_IMAGE_ATTACHMENT_DIMENSION`）。
const MAX_DIMENSION: u32 = 2000;
/// JPEG 的质量档（Node `JPEG_QUALITY_STEPS`）。
const JPEG_QUALITY_STEPS: [u8; 4] = [80, 60, 40, 20];
/// 缩到 2000 之后还不行时再缩的比例（Node `PROGRESSIVE_SCALE_FACTORS`）。
const SCALE_FACTORS: [f64; 3] = [0.75, 0.5, 0.25];
/// 最后的几档最长边与它们的 JPEG 质量（Node `AGGRESSIVE_JPEG_MAX_EDGES`，质量 20）。
const LAST_RESORT_EDGES: [u32; 6] = [1000, 800, 600, 400, 300, 200];
const LAST_RESORT_QUALITY: u8 = 20;
/// 按源格式重编 JPEG 时的质量（Jimp `getBuffer("image/jpeg")` 的缺省）。
const JPEG_DEFAULT_QUALITY: u8 = 100;
/// BMP 的文件头。
const BMP_MAGIC: &[u8] = b"BM";

/// 缩放的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prepared {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
    /// 重新编过（假：原样的字节）。
    pub changed: bool,
}

/// 不能发给模型的图片（Node 的 `ImageProcessorErrorCode`；调用方换成占位文字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrepareError {
    Empty,
    /// 认不出格式或解不开。
    Undecodable,
    /// 所有候选都超预算。
    TooLarge,
    /// WebP 超预算：照 Node 不转码。
    WebpTooLarge,
}

/// 要解码的格式：发给模型的 PNG、JPEG、GIF 加 BMP（Node 的 Jimp 能解 BMP）。WebP 不解码（照 Node 不转码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
    Gif,
    Bmp,
}

impl Format {
    fn detect(bytes: &[u8]) -> Option<Self> {
        match rust_cli_kernel_api::image::media_type(bytes) {
            Some(mime::PNG) => Some(Self::Png),
            Some(mime::JPEG) => Some(Self::Jpeg),
            Some(mime::GIF) => Some(Self::Gif),
            _ => bytes.starts_with(BMP_MAGIC).then_some(Self::Bmp),
        }
    }

    fn media_type(self) -> &'static str {
        match self {
            Self::Png => mime::PNG,
            Self::Jpeg => mime::JPEG,
            Self::Gif => mime::GIF,
            Self::Bmp => mime::BMP,
        }
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Gif => ImageFormat::Gif,
            Self::Bmp => ImageFormat::Bmp,
        }
    }
}

/// 找候选时的源格式（Node 的 `sourceMediaType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Png,
    Jpeg,
    Gif,
}

/// 字节在预算内（base64 之后也就不超过 5 MiB：3,932,160 × 4 / 3 正好是 5 MiB）。
fn fits(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_IMAGE_BYTES
}

/// 缩放（session-lifecycle.md §10.41 引的规则）。
pub(crate) fn prepare(bytes: Vec<u8>) -> Result<Prepared, PrepareError> {
    if bytes.is_empty() {
        return Err(PrepareError::Empty);
    }
    // WebP 不转码（Node `prepareWebpPassthrough`）：在预算内原样，超了就不发
    if rust_cli_kernel_api::image::media_type(&bytes) == Some(mime::WEBP) {
        return if fits(&bytes) {
            Ok(Prepared {
                media_type: mime::WEBP,
                bytes,
                changed: false,
            })
        } else {
            Err(PrepareError::WebpTooLarge)
        };
    }
    let format = Format::detect(&bytes).ok_or(PrepareError::Undecodable)?;
    let image = decode(&bytes, format).ok_or(PrepareError::Undecodable)?;
    if longest_edge(&image) <= MAX_DIMENSION && fits(&bytes) {
        return Ok(Prepared {
            media_type: format.media_type(),
            bytes,
            changed: false,
        });
    }
    let source = match format {
        Format::Jpeg => Source::Jpeg,
        Format::Gif => Source::Gif,
        // 不是四种之一的 Node 按 PNG 处理（`normalizeMediaType`）：BMP 之后的候选同 PNG
        Format::Png | Format::Bmp => Source::Png,
    };
    let (media_type, bytes) = search(&image, source).ok_or(PrepareError::TooLarge)?;
    Ok(Prepared {
        media_type,
        bytes,
        changed: true,
    })
}

/// 解码，按 JPEG 的 EXIF 方向转正（重编之后没有 EXIF，不转正就是歪的）。
fn decode(bytes: &[u8], format: Format) -> Option<DynamicImage> {
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format.image_format())
        .into_decoder()
        .ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some(image)
}

fn longest_edge(image: &DynamicImage) -> u32 {
    image.width().max(image.height())
}

/// 缩到最长边不超过 `max_edge`（Jimp `scaleToFit` 的取整：`round(边 × 比例)`、至少 1；双三次）。
fn fit(image: &DynamicImage, max_edge: u32) -> DynamicImage {
    let longest = longest_edge(image);
    if longest <= max_edge {
        return image.clone();
    }
    let factor = f64::from(max_edge) / f64::from(longest);
    let scaled = |edge: u32| ((f64::from(edge) * factor).round() as u32).max(1);
    image.resize_exact(
        scaled(image.width()),
        scaled(image.height()),
        FilterType::CatmullRom,
    )
}

type Candidate = (&'static str, Vec<u8>);

/// 在预算内的候选；编不出来的当作不行。
fn fitting(candidate: Option<Candidate>) -> Option<Candidate> {
    candidate.filter(|(_, bytes)| fits(bytes))
}

/// Node `findFirstFittingCandidate` 的顺序。
fn search(image: &DynamicImage, source: Source) -> Option<Candidate> {
    let within = longest_edge(image) <= MAX_DIMENSION;
    // PNG 只有一次原尺寸的无损机会，之后只转 JPEG（Node 的注释：否则低分辨率的 PNG 先于较大的 JPEG 命中）
    let preserve = source != Source::Png;
    if within && let Some(found) = keep_format(image, source) {
        return Some(found);
    }
    let bounded = fit(image, MAX_DIMENSION);
    // 缩到 2000 之后：按源格式编（Jimp 的缺省选项），再试保持格式的那几档
    if preserve && !within {
        if let Some(found) = fitting(plain(&bounded, source)) {
            return Some(found);
        }
        if let Some(found) = keep_format(&bounded, source) {
            return Some(found);
        }
    }
    if let Some(found) = jpeg_steps(&bounded) {
        return Some(found);
    }
    for scale in SCALE_FACTORS {
        let edge = ((f64::from(longest_edge(&bounded)) * scale).round() as u32).max(1);
        let scaled = fit(&bounded, edge);
        if preserve && let Some(found) = keep_format(&scaled, source) {
            return Some(found);
        }
        if let Some(found) = jpeg_steps(&scaled) {
            return Some(found);
        }
    }
    LAST_RESORT_EDGES.into_iter().find_map(|edge| {
        let scaled = fit(image, edge.min(MAX_DIMENSION));
        fitting(jpeg(&scaled, LAST_RESORT_QUALITY))
    })
}

/// 保持格式（Node `findFormatPreservingCandidate`）：PNG 用最高压缩重编，JPEG 走质量档，GIF 重编。
fn keep_format(image: &DynamicImage, source: Source) -> Option<Candidate> {
    match source {
        Source::Png => fitting(png(image)),
        Source::Jpeg => jpeg_steps(image),
        Source::Gif => fitting(gif(image)),
    }
}

fn jpeg_steps(image: &DynamicImage) -> Option<Candidate> {
    JPEG_QUALITY_STEPS
        .into_iter()
        .find_map(|quality| fitting(jpeg(image, quality)))
}

/// 按源格式、Jimp 的缺省选项编（Jimp `getBuffer(<源格式>)`：PNG 是 deflate 9，JPEG 是质量 100）。
fn plain(image: &DynamicImage, source: Source) -> Option<Candidate> {
    match source {
        Source::Png => png(image),
        Source::Jpeg => jpeg(image, JPEG_DEFAULT_QUALITY),
        Source::Gif => gif(image),
    }
}

/// PNG：最高压缩、自适应滤波（Jimp 的 deflate 9）。
fn png(image: &DynamicImage) -> Option<Candidate> {
    let mut out = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut out, CompressionType::Best, PngFilter::Adaptive);
    image.write_with_encoder(encoder).ok()?;
    Some((mime::PNG, out))
}

/// JPEG 没有透明通道：去掉 alpha（同 Jimp 的 JPEG 编码只取 RGB）。
fn jpeg(image: &DynamicImage, quality: u8) -> Option<Candidate> {
    let mut out = Vec::new();
    let rgb = DynamicImage::ImageRgb8(image.to_rgb8());
    rgb.write_with_encoder(JpegEncoder::new_with_quality(&mut out, quality))
        .ok()?;
    Some((mime::JPEG, out))
}

/// GIF：只有一帧（解码时取的就是第一帧，同 Jimp）。
fn gif(image: &DynamicImage) -> Option<Candidate> {
    let mut out = Vec::new();
    GifEncoder::new(&mut out)
        .encode_frame(Frame::new(image.to_rgba8()))
        .ok()?;
    Some((mime::GIF, out))
}

#[cfg(test)]
#[path = "attachment_image_tests.rs"]
mod tests;
