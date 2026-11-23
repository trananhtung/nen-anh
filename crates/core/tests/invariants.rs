//! What the app promises, checked on synthetic pictures.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use nenanh_core::batch::{self, Report};
use nenanh_core::{Error, Level, Options, Outcome, Target, compress};

const ALL_LEVELS: [Level; 4] = [Level::Lossless, Level::High, Level::Balanced, Level::Small];
const ALL_TARGETS: [Target; 3] = [Target::Keep, Target::Webp, Target::Avif];

fn opts(target: Target, level: Level) -> Options {
    Options { target, level, strip_metadata: false }
}

fn run(input: &[u8], o: Options) -> Outcome {
    compress(input, o, &AtomicBool::new(false)).expect("compress")
}

/// Smooth shapes plus mild noise: behaves like a photo.
fn photo(w: u32, h: u32) -> RgbImage {
    let mut seed = 7u32;
    RgbImage::from_fn(w, h, |x, y| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let n = (seed >> 29) as f32;
        let (fx, fy) = (x as f32 / 23.0, y as f32 / 31.0);
        let v = |k: f32| (128.0 + 70.0 * (fx * k).sin() + 45.0 * (fy + k).cos() + n) as u8;
        Rgb([v(1.0), v(0.7), v(0.4)])
    })
}

fn encode(img: impl Into<DynamicImage>, f: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.into().write_to(&mut out, f).unwrap();
    out.into_inner()
}

fn jpeg(img: RgbImage, q: u8) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q).encode_image(&img).unwrap();
    out
}

fn sample_jpeg() -> Vec<u8> {
    jpeg(photo(321, 243), 96)
}

fn logo() -> RgbaImage {
    RgbaImage::from_fn(200, 120, |x, y| {
        let d = ((x as f32 - 100.0).powi(2) + (y as f32 - 60.0).powi(2)).sqrt();
        // Colour is deliberately kept under fully transparent pixels.
        Rgba([x as u8, 40, (y * 2) as u8, (255.0 - (d - 40.0).clamp(0.0, 25.0) * 10.2) as u8])
    })
}

fn with_segment(jpeg: &[u8], marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, marker];
    v.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    v.extend_from_slice(payload);
    v.extend_from_slice(&jpeg[2..]);
    v
}

fn exif(orientation: u8) -> Vec<u8> {
    let mut v = b"Exif\0\0".to_vec();
    v.extend_from_slice(&[b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 1, 0x12, 0, 3, 0, 0, 0, 1, 0, orientation, 0, 0]);
    v.extend_from_slice(&[0, 0, 0, 0]);
    v.extend_from_slice(b"GPS-SECRET");
    v
}

fn contains(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

/// `(width, height)` from the `ispe` box.
fn avif_size(d: &[u8]) -> (u32, u32) {
    let p = d.windows(4).position(|w| w == b"ispe").expect("ispe") + 8;
    let n = |o: usize| u32::from_be_bytes(d[o..o + 4].try_into().unwrap());
    (n(p), n(p + 4))
}

#[test]
fn dimensions_never_change_and_output_is_never_larger() {
    let inputs = [
        sample_jpeg(),
        encode(photo(160, 90), ImageFormat::Png),
        encode(logo(), ImageFormat::Png),
        encode(logo(), ImageFormat::WebP),
        encode(RgbImage::from_pixel(1, 1, Rgb([9, 9, 9])), ImageFormat::Png),
        jpeg(photo(5, 3), 90),
    ];
    for (i, input) in inputs.iter().enumerate() {
        let size = image::load_from_memory(input).unwrap().dimensions();
        for t in ALL_TARGETS {
            for l in ALL_LEVELS {
                let out = run(input, opts(t, l));
                let Some(data) = out.data else { continue };
                assert!(data.len() < input.len(), "#{i} {t:?} {l:?} grew");
                let got = if out.ext == Some("avif") {
                    avif_size(&data)
                } else {
                    image::load_from_memory(&data).unwrap().dimensions()
                };
                assert_eq!(got, size, "#{i} {t:?} {l:?}");
                if let Some(s) = out.score {
                    assert!(s >= l.floor(), "#{i} {t:?} {l:?} scored {s}");
                }
            }
        }
    }
}

#[test]
fn lossless_keeps_every_pixel() {
    for input in [
        sample_jpeg(),
        encode(photo(160, 90), ImageFormat::Png),
        encode(logo(), ImageFormat::Png),
    ] {
        let out = run(&input, opts(Target::Keep, Level::Lossless));
        assert!(out.lossless);
        let (a, b) = (image::load_from_memory(&input).unwrap(), image::load_from_memory(&out.data.unwrap()).unwrap());
        assert!(a.to_rgba8() == b.to_rgba8());
    }
    let input = encode(logo(), ImageFormat::Png);
    let out = run(&input, opts(Target::Webp, Level::Lossless));
    let data = out.data.expect("webp smaller than png");
    assert!(image::load_from_memory(&data).unwrap().to_rgba8() == logo(), "hidden colours must survive");
}

#[test]
fn lossy_levels_save_more_than_lossless() {
    let input = sample_jpeg();
    let size = |l| run(&input, opts(Target::Keep, l)).data.map_or(input.len(), |d| d.len());
    let sizes = ALL_LEVELS.map(size);
    assert!(sizes[0] < input.len());
    assert!(sizes[2] < sizes[0], "{sizes:?}");
    assert!(sizes[3] <= sizes[2] && sizes[2] <= sizes[1], "{sizes:?}");
}

#[test]
fn second_pass_does_not_grind_the_picture_down() {
    let once = run(&sample_jpeg(), opts(Target::Keep, Level::Lossless)).data.unwrap();
    let twice = run(&once, opts(Target::Keep, Level::Lossless));
    assert!(twice.data.is_none_or(|d| d.len() < once.len()));
}

#[test]
fn sixteen_bit_png_is_never_truncated() {
    let img = image::ImageBuffer::<Rgb<u16>, _>::from_fn(80, 60, |x, y| {
        Rgb([(x * 777 + 3) as u16, (y * 901 + 5) as u16, (x * y) as u16])
    });
    let input = encode(img.clone(), ImageFormat::Png);
    for t in ALL_TARGETS {
        for l in ALL_LEVELS {
            let out = run(&input, opts(t, l));
            assert_eq!(out.ext, None);
            let data = out.data.unwrap_or(input.clone());
            assert!(image::load_from_memory(&data).unwrap().to_rgb16() == img, "{t:?} {l:?}");
        }
    }
}

#[test]
fn metadata_is_kept_by_default_and_removed_on_request() {
    let input = with_segment(&sample_jpeg(), 0xE1, &exif(6));
    let input = with_segment(&input, 0xE2, b"ICC_PROFILE\0\x01\x01fake sRGB profile");
    for t in [Target::Keep, Target::Webp] {
        for l in [Level::Lossless, Level::Balanced] {
            let kept = run(&input, opts(t, l)).data.unwrap_or(input.clone());
            assert!(contains(&kept, b"GPS-SECRET"), "{t:?} {l:?}");
            assert!(contains(&kept, b"fake sRGB profile"));
            let o = Options { strip_metadata: true, ..opts(t, l) };
            let out = run(&input, o);
            let data = out.data.expect("stripping always rewrites a plain JPEG");
            assert!(!contains(&data, b"GPS-SECRET"), "{t:?} {l:?}");
            assert!(contains(&data, b"fake sRGB profile"));
            if out.ext.is_none() {
                assert!(contains(&data, &[1, 0x12, 0, 3, 0, 0, 0, 1, 0, 6]), "orientation lost");
            }
        }
    }
}

#[test]
fn rotated_photos_are_baked_upright_when_converted() {
    let input = with_segment(&sample_jpeg(), 0xE1, &exif(6));
    let out = run(&input, opts(Target::Webp, Level::Balanced));
    assert_eq!(out.ext, Some("webp"));
    let data = out.data.unwrap();
    assert_eq!(image::load_from_memory(&data).unwrap().dimensions(), (243, 321));
    assert!(contains(&data, &[1, 0x12, 0, 3, 0, 0, 0, 1, 0, 1]), "must not rotate twice");
    let out = run(&input, opts(Target::Avif, Level::Balanced));
    assert_eq!(avif_size(&out.data.unwrap()), (243, 321));
}

#[test]
fn pictures_with_companion_data_are_left_alone() {
    let input = with_segment(&sample_jpeg(), 0xE1, b"http://ns.adobe.com/xap/1.0/\0<x hdrgm:Version=\"1.0\"/>");
    for t in ALL_TARGETS {
        let out = run(&input, Options { strip_metadata: true, ..opts(t, Level::Small) });
        assert!(out.data.is_none() && out.metadata_left);
    }
}

#[test]
fn animated_png_is_left_alone() {
    let mut png = encode(logo(), ImageFormat::Png);
    // acTL right after IHDR (8 + 25 bytes); the CRC is irrelevant for detection.
    let chunk = [&[0, 0, 0, 8][..], b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0], &[0; 4]].concat();
    png.splice(33..33, chunk);
    for t in ALL_TARGETS {
        assert!(run(&png, opts(t, Level::Small)).data.is_none());
    }
}

#[test]
fn transparency_outside_the_sampled_tiles_is_handled() {
    let base = photo(2100, 2100);
    let img = RgbaImage::from_fn(2100, 2100, |x, y| {
        let Rgb([r, g, b]) = *base.get_pixel(x, y);
        Rgba([r, g, b, if (600..700).contains(&x) && y < 100 { 0 } else { 255 }])
    });
    let input = encode(img, ImageFormat::Png);
    let out = run(&input, opts(Target::Webp, Level::Balanced));
    assert_eq!(out.ext, Some("webp"));
    let got = image::load_from_memory(&out.data.unwrap()).unwrap().to_rgba8();
    assert_eq!(got.get_pixel(650, 50)[3], 0);
    assert_eq!(got.get_pixel(50, 50)[3], 255);
}

#[test]
fn opaque_webp_with_alpha_channel_is_handled() {
    let img = DynamicImage::ImageRgb8(photo(200, 150)).to_rgba8();
    let input = encode(img, ImageFormat::WebP);
    for l in ALL_LEVELS {
        assert!(run(&input, opts(Target::Keep, l)).data.is_some(), "{l:?}");
    }
}

#[test]
fn non_srgb_pictures_do_not_become_avif() {
    let input = with_segment(&sample_jpeg(), 0xE2, b"ICC_PROFILE\0\x01\x01Display P3");
    let out = run(&input, opts(Target::Avif, Level::Balanced));
    assert_eq!(out.ext, None);
    assert!(contains(&out.data.unwrap(), b"Display P3"));
}

#[test]
fn damaged_files_are_errors_not_crashes() {
    let j = sample_jpeg();
    let p = encode(photo(64, 64), ImageFormat::Png);
    let w = encode(logo(), ImageFormat::WebP);
    let bad = [j[..j.len() / 2].to_vec(), p[..p.len() / 2].to_vec(), w[..w.len() / 2].to_vec(), vec![], b"GIF89a".to_vec()];
    for (i, input) in bad.iter().enumerate() {
        for t in ALL_TARGETS {
            let r = compress(input, opts(t, Level::Balanced), &AtomicBool::new(false));
            assert!(matches!(r, Err(Error::Failed(_))), "#{i} {t:?}: {r:?}");
        }
    }
    let mut noise = j.clone();
    for i in (300..noise.len()).step_by(97) {
        noise[i] ^= 0x5A;
    }
    let _ = compress(&noise, opts(Target::Keep, Level::Balanced), &AtomicBool::new(false));
}

#[test]
fn cancelling_stops_the_work() {
    let r = compress(&sample_jpeg(), opts(Target::Keep, Level::Balanced), &AtomicBool::new(true));
    assert_eq!(r.unwrap_err(), Error::Cancelled);
}

#[test]
fn batch_is_safe_for_originals_and_survives_bad_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Ảnh cưới Đà Lạt");
    fs::create_dir_all(root.join("con/cháu")).unwrap();
    fs::create_dir_all(root.join("anh-nen")).unwrap();
    let files: [(&str, Vec<u8>); 6] = [
        ("a.jpg", sample_jpeg()),
        ("a.png", encode(logo(), ImageFormat::Png)),
        ("con/cháu/ảnh đẹp.JPG", sample_jpeg()),
        ("hỏng.jpg", sample_jpeg()[..900].to_vec()),
        ("sai-đuôi.png", sample_jpeg()),
        ("anh-nen/cũ.jpg", sample_jpeg()),
    ];
    for (name, data) in &files {
        fs::write(root.join(name), data).unwrap();
    }
    fs::write(root.join("ghi chú.txt"), "x").unwrap();

    let items = batch::scan(&[root.clone(), root.join("a.jpg")]);
    assert_eq!(items.len(), 5, "old output, text files and duplicates are skipped");

    let done = Mutex::new(Vec::<PathBuf>::new());
    let failed = Mutex::new(0);
    for _ in 0..2 {
        batch::run(&items, None, opts(Target::Webp, Level::Balanced), &AtomicBool::new(false), &|_, r| match r {
            Report::Done { output, before, after, .. } => {
                assert!(after <= before);
                done.lock().unwrap().push(output);
            }
            Report::Failed { .. } => *failed.lock().unwrap() += 1,
            _ => {}
        });
    }
    for (name, data) in &files {
        assert!(fs::read(root.join(name)).unwrap() == *data, "{name} was modified");
    }
    let mut done = done.into_inner().unwrap();
    assert_eq!((done.len(), *failed.lock().unwrap()), (8, 2));
    done.sort();
    done.dedup();
    assert_eq!(done.len(), 8, "no result overwrote another");
    assert!(root.join("anh-nen/con/cháu/ảnh đẹp.webp").exists());
    let leftovers = walk(&root).into_iter().filter(|p| p.to_string_lossy().contains(".nenanh-")).count();
    assert_eq!(leftovers, 0);
}

#[test]
fn forecast_does_not_promise_too_much() {
    use nenanh_core::estimate::estimate;
    let no = AtomicBool::new(false);
    let input = jpeg(photo(1400, 1100), 96);
    for t in [Target::Keep, Target::Webp] {
        let guess = estimate(&input, t, &no).unwrap();
        let mut limit = input.len() as f64;
        for (g, l) in guess.into_iter().zip(ALL_LEVELS) {
            assert!(g > 0.0 && g <= limit, "{t:?} {guess:?}");
            limit = g;
            if l == Level::Balanced {
                let real = run(&input, opts(t, l)).data.map_or(input.len(), |d| d.len()) as f64;
                let (said, got) = (1.0 - g / input.len() as f64, 1.0 - real / input.len() as f64);
                assert!(said < got + 0.05, "{t:?}: promised {said:.2}, got {got:.2}");
                assert!(said > got * 0.5, "{t:?}: promised only {said:.2}, got {got:.2}");
            }
        }
    }
    let hdr = with_segment(&sample_jpeg(), 0xE1, b"http://ns.adobe.com/xap/1.0/\0<x hdrgm:Version=\"1.0\"/>");
    assert_eq!(estimate(&hdr, Target::Webp, &no).unwrap(), [hdr.len() as f64; 4]);
    assert!(estimate(b"not an image", Target::Keep, &no).is_err());
    assert_eq!(estimate(&input, Target::Keep, &AtomicBool::new(true)).unwrap_err(), Error::Cancelled);
}

#[test]
fn forecast_skips_unreadable_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.jpg"), sample_jpeg()).unwrap();
    fs::write(dir.path().join("b.jpg"), &sample_jpeg()[..900]).unwrap();
    let items = batch::scan(&[dir.path().to_path_buf()]);
    let no = AtomicBool::new(false);
    let f = batch::forecast(&items, Target::Keep, &no).unwrap();
    assert_eq!(f.sampled, 1);
    assert!(f.after[2] < f.before);
    assert!(batch::forecast(&[], Target::Keep, &no).is_none());
    assert!(batch::forecast(&items, Target::Keep, &AtomicBool::new(true)).is_none());
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        if e.path().is_dir() { out.extend(walk(&e.path())) } else { out.push(e.path()) }
    }
    out
}
