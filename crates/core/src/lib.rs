//! Shrinks image files without touching their pixel dimensions.

mod avif;
pub mod batch;
mod codec;
mod container;

use std::sync::atomic::{AtomicBool, Ordering};

use codec::Raster;
use container::Metadata;
use serde::{Deserialize, Serialize};

const MAX_PIXELS: usize = 200_000_000;
/// Real AVIF decoders round 10-bit samples differently and score up to 2.6 points lower than the
/// encoder's own reconstruction (measured against libavif).
const AVIF_MARGIN: f64 = 3.0;
const WEBP_MAX_SIDE: usize = 16_383;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Keep,
    Webp,
    Avif,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Lossless,
    High,
    Balanced,
    Small,
}

impl Level {
    /// Minimum SSIMULACRA2 score a lossy result must reach.
    pub fn floor(self) -> f64 {
        match self {
            Level::Lossless => 100.0,
            Level::High => 90.0,
            Level::Balanced => 85.0,
            Level::Small => 75.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub target: Target,
    pub level: Level,
    pub strip_metadata: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Failed(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Failed(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Failed(s.into())
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// `None` means the original is already the best version.
    pub data: Option<Vec<u8>>,
    pub ext: Option<&'static str>,
    /// Every decoded pixel is identical to the original.
    pub lossless: bool,
    pub score: Option<f64>,
    pub note: Option<&'static str>,
    /// Metadata removal was requested but the original had to be kept as it is.
    pub metadata_left: bool,
}

impl Outcome {
    fn untouched(note: &'static str) -> Self {
        Outcome { note: Some(note), ..Default::default() }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Jpeg,
    Png,
    Webp,
}

pub fn sniff(data: &[u8]) -> Option<Kind> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if data.len() > 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        Some(Kind::Webp)
    } else {
        None
    }
}

const NOT_SMALLER: &str = "Ảnh đã tối ưu sẵn, giữ nguyên bản gốc";
const CONVERSION_USELESS: &str = "Đổi định dạng không nhẹ hơn, giữ định dạng gốc";
const ANIMATED: &str = "Ảnh động, giữ nguyên";
const COMPANION: &str = "Ảnh có dữ liệu HDR hoặc video đi kèm, giữ nguyên";

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) }
}

/// mozjpeg reports fatal errors by unwinding; turn that into an ordinary error.
fn guarded<T>(f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("File ảnh bị hỏng, không đọc được".into()))
}

/// Pixel count read from the header only, used to budget memory before decoding.
pub fn pixel_count(data: &[u8]) -> Result<usize, Error> {
    guarded(|| header_pixels(data))
}

fn header_pixels(data: &[u8]) -> Result<usize, Error> {
    let (w, h) = match sniff(data).ok_or("Định dạng không hỗ trợ (chỉ nhận JPG, PNG, WebP)")? {
        Kind::Jpeg => {
            let i = codec::jpeg_info(data)?;
            (i.w, i.h)
        }
        Kind::Png => match data.get(16..24) {
            Some(b) => (
                u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize,
                u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize,
            ),
            None => return Err("Không đọc được file PNG".into()),
        },
        Kind::Webp => codec::webp_size(data).ok_or("Không đọc được file WebP")?,
    };
    if w == 0 || h == 0 {
        return Err("Ảnh không có kích thước hợp lệ".into());
    }
    match w.checked_mul(h) {
        Some(n) if n <= MAX_PIXELS => Ok(n),
        _ => Err("Ảnh quá lớn (trên 200 megapixel)".into()),
    }
}

/// Compresses one picture held in memory. Never returns data that is not smaller than `input`
/// unless metadata removal was requested.
pub fn compress(input: &[u8], opts: Options, cancel: &AtomicBool) -> Result<Outcome, Error> {
    guarded(|| compress_inner(input, opts, cancel))
}

fn compress_inner(input: &[u8], opts: Options, cancel: &AtomicBool) -> Result<Outcome, Error> {
    pixel_count(input)?;
    check(cancel)?;
    let kind = sniff(input).ok_or("Định dạng không hỗ trợ")?;
    let mut out = match opts.target {
        Target::Keep => keep_format(kind, input, opts, cancel)?,
        _ => match decode_for_conversion(kind, input)? {
            Ok((raster, meta)) => {
                let out = convert(raster, meta, opts, cancel)?;
                if out.data.as_ref().is_some_and(|d| d.len() < input.len()) {
                    out
                } else {
                    let note = out.note.or(Some(CONVERSION_USELESS));
                    Outcome { note, ..keep_format(kind, input, opts, cancel)? }
                }
            }
            Err(why) if why == ANIMATED || why == COMPANION => Outcome::untouched(why),
            Err(why) => Outcome { note: Some(why), ..keep_format(kind, input, opts, cancel)? },
        },
    };
    if out.data.as_ref().is_some_and(|d| d.len() >= input.len() && !opts.strip_metadata) {
        out = Outcome { note: out.note, ..Default::default() };
    }
    if out.data.is_none() {
        out.note = out.note.or(Some(NOT_SMALLER));
        // The caller keeps the original bytes, so say plainly that nothing was removed.
        out.metadata_left = opts.strip_metadata;
    }
    Ok(out)
}

fn keep_format(kind: Kind, input: &[u8], opts: Options, cancel: &AtomicBool) -> Result<Outcome, Error> {
    match kind {
        Kind::Jpeg => jpeg(input, opts, cancel),
        Kind::Png => png(input, opts, cancel),
        Kind::Webp => webp(input, opts, cancel),
    }
}

/// Highest-compression quality in `lo..=hi` whose result still reaches the floor.
fn search(
    raster: &Raster,
    floor: f64,
    cancel: &AtomicBool,
    attempt: &dyn Fn(&Raster, u8, bool) -> Result<(Vec<u8>, Raster), String>,
) -> Result<Option<(Vec<u8>, f64)>, Error> {
    let proxy = raster.proxy();
    let probe = proxy.as_ref().unwrap_or(raster);
    let (mut lo, mut hi, mut best) = (40u8, 96u8, None);
    while lo <= hi {
        check(cancel)?;
        let mid = (lo + hi) / 2;
        if codec::score(probe, &attempt(probe, mid, false)?.1)? >= floor {
            best = Some(mid);
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    drop(proxy);
    let Some(mut quality) = best else { return Ok(None) };
    // The proxy is only a sample: confirm on the whole picture, stepping up until the floor is
    // met, or down while there is quality to spare.
    let mut accepted: Option<(Vec<u8>, f64)> = None;
    for _ in 0..5 {
        check(cancel)?;
        let (data, decoded) = attempt(raster, quality, true)?;
        let score = codec::score(raster, &decoded)?;
        if score < floor {
            if accepted.is_some() || quality >= 97 {
                break;
            }
            quality = (quality + 3).min(97);
            continue;
        }
        let done = score < floor + 1.0 || quality < 43 || accepted.as_ref().is_some_and(|(d, _)| d.len() <= data.len());
        if accepted.as_ref().is_none_or(|(d, _)| data.len() < d.len()) {
            accepted = Some((data, score));
        }
        if done {
            break;
        }
        quality -= 3;
    }
    Ok(accepted)
}

fn smaller(a: Outcome, b: Outcome) -> Outcome {
    match (&a.data, &b.data) {
        (Some(x), Some(y)) if y.len() < x.len() => b,
        (None, Some(_)) => b,
        _ => a,
    }
}

fn jpeg(input: &[u8], opts: Options, cancel: &AtomicBool) -> Result<Outcome, Error> {
    if container::jpeg_has_companion(input) {
        return Ok(Outcome::untouched(COMPANION));
    }
    let info = codec::jpeg_info(input)?;
    let finish = |encoded: &[u8]| {
        container::jpeg_with_metadata(encoded, input, opts.strip_metadata)
            .ok_or(Error::from("Không ghép được thông tin ảnh"))
    };
    // Also rejects truncated files, which decoders would silently pad with grey.
    let packed = finish(&codec::jpeg_transcode(input)?)?;
    let lossless = if codec::jpeg_same_pixels(input, &packed, info.components)? {
        Outcome { data: Some(packed), lossless: true, score: Some(100.0), ..Default::default() }
    } else {
        Outcome::default()
    };
    if opts.level == Level::Lossless {
        return Ok(lossless);
    }
    if !matches!(info.components, 1 | 3) {
        return Ok(Outcome { note: Some("Ảnh CMYK, chỉ nén không mất dữ liệu"), ..lossless });
    }
    check(cancel)?;
    let raster = codec::jpeg_decode(input)?;
    let found = search(
        &raster,
        opts.level.floor(),
        cancel,
        &|r, q, last| {
            let data = codec::jpeg_encode(r, &info, q, last)?;
            let decoded = codec::jpeg_decode(&data)?;
            Ok((data, decoded))
        },
    )?;
    Ok(match found {
        Some((data, score)) => {
            let data = finish(&data)?;
            smaller(lossless, Outcome { data: Some(data), score: Some(score), ..Default::default() })
        }
        None => lossless,
    })
}

fn png_icc(input: &[u8]) -> Option<Vec<u8>> {
    use image::ImageDecoder;
    image::codecs::png::PngDecoder::new(std::io::Cursor::new(input)).ok()?.icc_profile().ok()?
}

fn png_meta(input: &[u8]) -> Metadata {
    let exif = container::png_chunks(input)
        .iter()
        .find(|(n, _)| n == b"eXIf")
        .map(|(_, body)| body.to_vec());
    Metadata { exif, icc: png_icc(input), xmp: None }
}

fn png_is_animated(input: &[u8]) -> bool {
    container::png_chunks(input).iter().any(|(n, _)| n == b"acTL")
}

fn png_is_deep(input: &[u8]) -> bool {
    input.get(24) == Some(&16)
}

fn png(input: &[u8], opts: Options, cancel: &AtomicBool) -> Result<Outcome, Error> {
    if png_is_animated(input) {
        return Ok(Outcome::untouched(ANIMATED));
    }
    let meta = png_meta(input);
    if opts.strip_metadata && meta.orientation() != 1 {
        return Ok(Outcome::untouched("PNG có hướng xoay EXIF, giữ nguyên"));
    }
    let packed = codec::png_optimize(input, opts.strip_metadata)?;
    let lossless = if codec::png_same_pixels(input, &packed)? {
        Outcome { data: Some(packed), lossless: true, score: Some(100.0), ..Default::default() }
    } else {
        Outcome::default()
    };
    if opts.level == Level::Lossless {
        return Ok(lossless);
    }
    if png_is_deep(input) {
        return Ok(Outcome { note: Some("Ảnh 16-bit, chỉ nén không mất dữ liệu"), ..lossless });
    }
    check(cancel)?;
    let img = codec::png_decode(input)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let raster = Raster { w, h, ch: 4, data: img.into_rgba8().into_raw() }.simplify();
    let reduced = codec::quantize(&raster)?;
    let score = codec::score(&raster, &reduced)?;
    if score < opts.level.floor() {
        return Ok(lossless);
    }
    check(cancel)?;
    let data = codec::png_encode(&reduced, input, meta.icc.as_deref(), opts.strip_metadata)?;
    let decoded = codec::png_decode(&data)?;
    if (decoded.width() as usize, decoded.height() as usize) != (w, h) {
        return Ok(lossless);
    }
    Ok(smaller(lossless, Outcome { data: Some(data), score: Some(score), ..Default::default() }))
}

fn webp(input: &[u8], opts: Options, cancel: &AtomicBool) -> Result<Outcome, Error> {
    let info = container::webp_info(input);
    if info.animated {
        return Ok(Outcome::untouched(ANIMATED));
    }
    let raster = codec::webp_decode(input)?.simplify();
    let meta = if opts.strip_metadata { info.meta.stripped() } else { info.meta };
    // Repacking the same bitstream: only useful when metadata was dropped.
    let wrap = |data: &[u8]| {
        container::webp_with_metadata(data, raster.w as u32, raster.h as u32, raster.ch == 4, &meta)
    };
    let mut best = Outcome {
        data: Some(wrap(input)),
        lossless: true,
        score: Some(100.0),
        ..Default::default()
    };
    if info.lossless {
        let data = codec::webp_encode(&raster, None, true)?;
        if codec::webp_decode(&data)?.data == raster.data {
            let packed = Outcome {
                data: Some(wrap(&data)),
                lossless: true,
                score: Some(100.0),
                ..Default::default()
            };
            best = smaller(best, packed);
        }
    }
    if opts.level != Level::Lossless
        && let Some((data, score)) = search_webp(&raster, opts.level, cancel)?
    {
        best = smaller(best, Outcome { data: Some(wrap(&data)), score: Some(score), ..Default::default() });
    }
    Ok(best)
}

fn search_webp(r: &Raster, level: Level, cancel: &AtomicBool) -> Result<Option<(Vec<u8>, f64)>, Error> {
    search(
        r,
        level.floor(),
        cancel,
        &|r, q, last| {
            let data = codec::webp_encode(r, Some(q), last)?;
            let decoded = codec::webp_decode(&data)?.with_channels(r.ch);
            Ok((data, decoded))
        },
    )
}

/// Upright 8-bit pixels plus metadata, or the reason why this picture must not change format.
#[allow(clippy::type_complexity)]
fn decode_for_conversion(
    kind: Kind,
    input: &[u8],
) -> Result<Result<(Raster, Metadata), &'static str>, Error> {
    let (raster, mut meta) = match kind {
        Kind::Jpeg => {
            if container::jpeg_has_companion(input) {
                return Ok(Err(COMPANION));
            }
            if !matches!(codec::jpeg_info(input)?.components, 1 | 3) {
                return Ok(Err("Ảnh CMYK, giữ nguyên định dạng"));
            }
            codec::jpeg_transcode(input)?; // rejects damaged files
            (codec::jpeg_decode(input)?, container::jpeg_metadata(input))
        }
        Kind::Png => {
            if png_is_animated(input) {
                return Ok(Err(ANIMATED));
            }
            if png_is_deep(input) {
                return Ok(Err("Ảnh 16-bit, giữ nguyên định dạng"));
            }
            let meta = png_meta(input);
            let chunks = container::png_chunks(input);
            let has = |n: &[u8; 4]| chunks.iter().any(|(c, _)| c == n);
            let odd_gamma = chunks.iter().any(|(n, b)| {
                n == b"gAMA" && b.len() == 4 && u32::from_be_bytes([b[0], b[1], b[2], b[3]]) != 45455
            });
            if has(b"cICP") || (odd_gamma && meta.icc.is_none() && !has(b"sRGB")) {
                return Ok(Err("Ảnh dùng hệ màu đặc biệt, giữ nguyên định dạng"));
            }
            let img = codec::png_decode(input)?;
            let (w, h) = (img.width() as usize, img.height() as usize);
            (Raster { w, h, ch: 4, data: img.into_rgba8().into_raw() }, meta)
        }
        Kind::Webp => {
            let info = container::webp_info(input);
            if info.animated {
                return Ok(Err(ANIMATED));
            }
            (codec::webp_decode(input)?, info.meta)
        }
    };
    let raster = raster.simplify().upright(meta.orientation());
    if let Some(exif) = &mut meta.exif {
        container::reset_orientation(exif);
    }
    Ok(Ok((raster, meta)))
}

fn convert(r: Raster, meta: Metadata, opts: Options, cancel: &AtomicBool) -> Result<Outcome, Error> {
    let meta = if opts.strip_metadata { meta.stripped() } else { meta };
    check(cancel)?;
    if opts.target == Target::Avif {
        if opts.level == Level::Lossless {
            return Ok(Outcome::untouched("AVIF không có mức Không mất dữ liệu, giữ nguyên"));
        }
        if !meta.is_srgb() {
            return Ok(Outcome::untouched("Ảnh có hệ màu khác sRGB, không đổi sang AVIF để tránh sai màu"));
        }
        let floor = opts.level.floor() + AVIF_MARGIN;
        let found = search(&r, floor, cancel, &|r, q, _| avif::encode(r, q, &meta))?;
        return Ok(match found {
            Some((data, score)) => Outcome {
                data: Some(data),
                ext: Some("avif"),
                score: Some(score - AVIF_MARGIN),
                ..Default::default()
            },
            None => Outcome::default(),
        });
    }
    if r.w > WEBP_MAX_SIDE || r.h > WEBP_MAX_SIDE {
        return Ok(Outcome::untouched("Cạnh ảnh trên 16383 px, WebP không hỗ trợ"));
    }
    let wrap = |d: &[u8]| container::webp_with_metadata(d, r.w as u32, r.h as u32, r.ch == 4, &meta);
    let packed = codec::webp_encode(&r, None, opts.level == Level::Lossless)?;
    let mut best = Outcome::default();
    if codec::webp_decode(&packed)?.data == r.data {
        best = Outcome {
            data: Some(wrap(&packed)),
            ext: Some("webp"),
            lossless: true,
            score: Some(100.0),
            ..Default::default()
        };
    }
    if opts.level != Level::Lossless
        && let Some((data, score)) = search_webp(&r, opts.level, cancel)?
    {
        let lossy = Outcome {
            data: Some(wrap(&data)),
            ext: Some("webp"),
            score: Some(score),
            ..Default::default()
        };
        best = smaller(best, lossy);
    }
    Ok(best)
}
