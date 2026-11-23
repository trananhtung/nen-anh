//! Predicts how small a picture will get at every level, from a small sample of its pixels.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use crate::codec::{self, Raster};
use crate::container::{self, Metadata};
use crate::{
    AVIF_MARGIN, ANIMATED, COMPANION, Error, Kind, Level, Target, WEBP_MAX_SIDE, avif, check,
    decode_for_conversion, guarded, pixel_count, png_is_animated, png_is_deep, sniff,
};

/// Predicted size in bytes for lossless, high, balanced and small, in that order.
pub type Sizes = [f64; 4];

/// Extra score demanded from the sample. The real search is stricter on whole pictures, and a
/// forecast that promises too little is better than one that promises too much.
const CAUTION: f64 = 3.0;

/// Quality at which the sample and the whole picture are compared.
const PROBE: u8 = 80;

const LEVELS: [Level; 4] = [Level::Lossless, Level::High, Level::Balanced, Level::Small];

/// Never larger than the original, and a stronger level is never larger than a weaker one.
fn tidy(mut sizes: Sizes, original: f64) -> Sizes {
    let mut limit = original;
    for s in &mut sizes {
        *s = s.min(limit);
        limit = *s;
    }
    sizes
}

pub fn estimate(input: &[u8], target: Target, cancel: &AtomicBool) -> Result<Sizes, Error> {
    guarded(|| {
        pixel_count(input)?;
        let kind = sniff(input).ok_or("Định dạng không hỗ trợ")?;
        let original = input.len() as f64;
        let sizes = match target {
            Target::Keep => keep(kind, input, cancel)?,
            _ => match decode_for_conversion(kind, input)? {
                Ok((raster, meta)) => {
                    let converted = convert(&raster, &meta, target, original, cancel)?;
                    drop(raster);
                    if converted.iter().all(|s| *s < original) {
                        converted
                    } else {
                        // The app falls back to the original format where converting is useless.
                        let kept = keep(kind, input, cancel)?;
                        std::array::from_fn(|i| if converted[i] < original { converted[i] } else { kept[i] })
                    }
                }
                Err(why) if why == ANIMATED || why == COMPANION => [original; 4],
                Err(_) => keep(kind, input, cancel)?,
            },
        };
        Ok(tidy(sizes, original))
    })
}

/// Size of the smallest encoding that reaches each lossy floor; infinite when none does.
fn lossy(
    sample: &Raster,
    scale: f64,
    margin: f64,
    cancel: &AtomicBool,
    attempt: &dyn Fn(&Raster, u8) -> Result<(Vec<u8>, Raster), String>,
) -> Result<[f64; 3], Error> {
    let mut seen: BTreeMap<u8, (f64, f64)> = BTreeMap::new();
    let mut out = [f64::INFINITY; 3];
    for (slot, level) in out.iter_mut().zip(&LEVELS[1..]) {
        let floor = level.floor() + margin + CAUTION;
        let (mut lo, mut hi) = (40u8, 96u8);
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if let std::collections::btree_map::Entry::Vacant(e) = seen.entry(mid) {
                check(cancel)?;
                let (data, decoded) = attempt(sample, mid)?;
                e.insert((data.len() as f64 * scale, codec::score(sample, &decoded)?));
            }
            let (size, score) = seen[&mid];
            if score >= floor {
                *slot = size;
                hi = mid - 1;
            } else {
                lo = mid + 1;
            }
        }
    }
    Ok(out)
}

fn with_lossless(lossless: f64, [a, b, c]: [f64; 3]) -> Sizes {
    [lossless, a, b, c]
}

/// Lossless sizes follow the area, lossy ones do not.
fn area(full: &Raster, sample: &Raster) -> f64 {
    (full.w * full.h) as f64 / (sample.w * sample.h) as f64
}

/// A sample, and how many times larger the whole picture is once encoded. Detail is rarely spread
/// evenly, so this compares two real encodings instead of two areas.
fn sampled(
    r: &Raster,
    cancel: &AtomicBool,
    encode: &dyn Fn(&Raster) -> Result<Vec<u8>, String>,
) -> Result<(Raster, f64), Error> {
    let sample = r.sample();
    if (sample.w, sample.h) == (r.w, r.h) {
        return Ok((sample, 1.0));
    }
    check(cancel)?;
    let scale = encode(r)?.len() as f64 / encode(&sample)?.len() as f64;
    Ok((sample, scale))
}

fn webp_sampled(r: &Raster, cancel: &AtomicBool) -> Result<(Raster, f64), Error> {
    sampled(r, cancel, &|r| codec::webp_encode(r, Some(PROBE), false))
}

fn webp_lossy(s: &Raster, scale: f64, cancel: &AtomicBool) -> Result<[f64; 3], Error> {
    lossy(s, scale, 0.0, cancel, &|r, q| {
        let data = codec::webp_encode(r, Some(q), false)?;
        let decoded = codec::webp_decode(&data)?.with_channels(r.ch);
        Ok((data, decoded))
    })
}

fn convert(
    r: &Raster,
    meta: &Metadata,
    target: Target,
    original: f64,
    cancel: &AtomicBool,
) -> Result<Sizes, Error> {
    const NEVER: f64 = f64::INFINITY;
    let fits_webp = r.w <= WEBP_MAX_SIDE && r.h <= WEBP_MAX_SIDE;
    if !fits_webp && target == Target::Webp || target == Target::Avif && !meta.is_srgb() {
        return Ok([NEVER; 4]);
    }
    // Encoding a whole picture to AVIF is slow; WebP finds detail in the same places.
    let (sample, scale) = if fits_webp {
        webp_sampled(r, cancel)?
    } else {
        let sample = r.sample();
        let scale = area(r, &sample);
        (sample, scale)
    };
    if target == Target::Avif {
        let none = Metadata::default();
        let sizes = lossy(&sample, scale, AVIF_MARGIN, cancel, &|r, q| avif::encode(r, q, &none))?;
        return Ok(with_lossless(NEVER, sizes));
    }
    check(cancel)?;
    // Encoding the whole picture takes seconds; skip it where the sample says it is hopeless.
    let mut lossless = codec::webp_lossless_fast(&sample)?.len() as f64 * area(r, &sample);
    if lossless < original * 2.0 {
        lossless = codec::webp_lossless_fast(r)?.len() as f64;
    }
    Ok(with_lossless(lossless, webp_lossy(&sample, scale, cancel)?))
}

fn keep(kind: Kind, input: &[u8], cancel: &AtomicBool) -> Result<Sizes, Error> {
    let original = input.len() as f64;
    match kind {
        Kind::Jpeg => {
            if container::jpeg_has_companion(input) {
                return Ok([original; 4]);
            }
            let info = codec::jpeg_info(input)?;
            let lossless = codec::jpeg_transcode(input)?.len() as f64;
            if !matches!(info.components, 1 | 3) {
                return Ok([lossless; 4]);
            }
            let (sample, scale) = sampled(&codec::jpeg_decode(input)?, cancel, &|r| {
                codec::jpeg_encode(r, &info, PROBE, false)
            })?;
            let sizes = lossy(&sample, scale, 0.0, cancel, &|r, q| {
                let data = codec::jpeg_encode(r, &info, q, false)?;
                let decoded = codec::jpeg_decode(&data)?;
                Ok((data, decoded))
            })?;
            Ok(with_lossless(lossless, sizes))
        }
        Kind::Png => {
            if png_is_animated(input) {
                return Ok([original; 4]);
            }
            let lossless = codec::png_optimize_fast(input)?.len() as f64;
            if png_is_deep(input) {
                return Ok([lossless; 4]);
            }
            check(cancel)?;
            let img = codec::png_decode(input)?;
            let (w, h) = (img.width() as usize, img.height() as usize);
            let full = Raster { w, h, ch: 4, data: img.into_rgba8().into_raw() }.simplify();
            // Whether 256 colours are enough is all or nothing, so a sample is not evidence.
            let reduced = codec::quantize(&full)?;
            let score = codec::score(&full, &reduced)?;
            drop(full);
            check(cancel)?;
            if score < Level::Small.floor() {
                return Ok([lossless; 4]);
            }
            let smaller = codec::png_encode_fast(&reduced)?.len() as f64;
            let at = |l: Level| if score >= l.floor() { smaller } else { lossless };
            Ok([lossless, at(Level::High), at(Level::Balanced), at(Level::Small)])
        }
        Kind::Webp => {
            let info = container::webp_info(input);
            if info.animated {
                return Ok([original; 4]);
            }
            let raster = codec::webp_decode(input)?.simplify();
            let (sample, scale) = webp_sampled(&raster, cancel)?;
            let lossless = if info.lossless {
                codec::webp_encode(&sample, None, true)?.len() as f64 * area(&raster, &sample)
            } else {
                original
            };
            Ok(with_lossless(lossless, webp_lossy(&sample, scale, cancel)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tidy;

    #[test]
    fn predictions_never_grow() {
        let inf = f64::INFINITY;
        assert_eq!(tidy([inf, 80.0, 90.0, 10.0], 100.0), [100.0, 80.0, 80.0, 10.0]);
    }
}
