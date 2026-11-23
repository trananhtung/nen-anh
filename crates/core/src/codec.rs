//! Thin wrappers around the encoders/decoders, plus the perceptual score.

use std::os::raw::{c_int, c_ulong};

use mozjpeg_sys as ffi;

use crate::container;

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Interleaved 8-bit pixels with 3 (RGB) or 4 (RGBA) channels.
#[derive(Clone)]
pub struct Raster {
    pub w: usize,
    pub h: usize,
    pub ch: usize,
    pub data: Vec<u8>,
}

impl Raster {
    fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Raster {
        let mut data = Vec::with_capacity(w * h * self.ch);
        for row in y..y + h {
            let start = (row * self.w + x) * self.ch;
            data.extend_from_slice(&self.data[start..start + w * self.ch]);
        }
        Raster { w, h, ch: self.ch, data }
    }

    /// Nine tiles spread over the picture, side by side.
    fn tiles(&self, tile: usize) -> Raster {
        let mut data = Vec::with_capacity(tile * tile * 9 * self.ch);
        let at = |i: usize, full: usize| ((full - tile) * i / 2) & !15;
        for ty in 0..3 {
            let y0 = at(ty, self.h);
            for row in y0..y0 + tile {
                for tx in 0..3 {
                    let start = (row * self.w + at(tx, self.w)) * self.ch;
                    data.extend_from_slice(&self.data[start..start + tile * self.ch]);
                }
            }
        }
        Raster { w: tile * 3, h: tile * 3, ch: self.ch, data }
    }

    /// A cheap stand-in while searching for a quality.
    pub fn proxy(&self) -> Option<Raster> {
        const TILE: usize = 512;
        (self.w >= TILE * 4 && self.h >= TILE * 4).then(|| self.tiles(TILE))
    }

    /// An even smaller stand-in for predicting sizes.
    pub fn sample(&self) -> Raster {
        let tile = (self.w.min(self.h) / 3).min(256) & !15;
        if self.w * self.h <= 600_000 || tile < 64 {
            return self.clone();
        }
        self.tiles(tile)
    }

    fn over(&self, bg: u8) -> Raster {
        let data = self
            .data
            .chunks_exact(4)
            .flat_map(|p| {
                let a = u32::from(p[3]);
                let mix = |c: u8| ((u32::from(c) * a + u32::from(bg) * (255 - a) + 127) / 255) as u8;
                [mix(p[0]), mix(p[1]), mix(p[2])]
            })
            .collect();
        Raster { w: self.w, h: self.h, ch: 3, data }
    }

    /// Drops an alpha channel that is fully opaque.
    pub fn simplify(self) -> Raster {
        if self.ch == 4 && self.data.chunks_exact(4).all(|p| p[3] == 255) {
            return self.over(0);
        }
        self
    }

    /// Encoders may drop an opaque alpha channel; put it back so pictures can be compared.
    pub fn with_channels(self, ch: usize) -> Raster {
        if self.ch == 3 && ch == 4 {
            let data = self.data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
            return Raster { w: self.w, h: self.h, ch, data };
        }
        self
    }

    /// Applies an EXIF orientation to the pixels.
    pub fn upright(self, orientation: u8) -> Raster {
        if !(2..=8).contains(&orientation) {
            return self;
        }
        let (w, h, ch) = (self.w, self.h, self.ch);
        let (nw, nh) = if orientation >= 5 { (h, w) } else { (w, h) };
        let mut data = Vec::with_capacity(self.data.len());
        for y in 0..nh {
            for x in 0..nw {
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                let i = (sy * w + sx) * ch;
                data.extend_from_slice(&self.data[i..i + ch]);
            }
        }
        Raster { w: nw, h: nh, ch, data }
    }
}

fn ssim2(a: &Raster, b: &Raster) -> Res<f64> {
    use fast_ssim2::{PixelDescriptor, PixelSlice, Ssimulacra2Config, compute_ssimulacra2_with_config};
    let view = |r: &'_ Raster| (r.w as u32, r.h as u32, r.w * 3);
    let (w, h, stride) = view(a);
    let d = PixelDescriptor::RGB8_SRGB;
    let a = PixelSlice::new(&a.data, w, h, stride, d).map_err(err)?;
    let b = PixelSlice::new(&b.data, w, h, stride, d).map_err(err)?;
    compute_ssimulacra2_with_config(&a, &b, &Ssimulacra2Config::strips(256)).map_err(err)
}

/// SSIMULACRA2 of two pictures. Very large ones are scored region by region to bound memory.
pub fn score(a: &Raster, b: &Raster) -> Res<f64> {
    if (a.w, a.h, a.ch, a.data.len()) != (b.w, b.h, b.ch, b.data.len()) {
        return Err("Kích thước ảnh không khớp".into());
    }
    if a.ch == 4 {
        let dark = score(&a.over(0), &b.over(0))?;
        return Ok(dark.min(score(&a.over(255), &b.over(255))?));
    }
    if a.w * a.h <= 40_000_000 {
        return ssim2(a, b);
    }
    const TILE: usize = 4096;
    let (nx, ny) = (a.w.div_ceil(TILE), a.h.div_ceil(TILE));
    let (mut sum, mut min) = (0.0, f64::MAX);
    for ty in 0..ny {
        for tx in 0..nx {
            let (x0, x1) = (a.w * tx / nx, a.w * (tx + 1) / nx);
            let (y0, y1) = (a.h * ty / ny, a.h * (ty + 1) / ny);
            let s = ssim2(&a.crop(x0, y0, x1 - x0, y1 - y0), &b.crop(x0, y0, x1 - x0, y1 - y0))?;
            sum += s;
            min = f64::min(min, s);
        }
    }
    Ok((sum / (nx * ny) as f64).min(min + 8.0))
}

// ---------------------------------------------------------------- JPEG

pub struct JpegInfo {
    pub w: usize,
    pub h: usize,
    pub components: usize,
    pub gray: bool,
    /// Chroma subsampling as (horizontal, vertical) pixel sizes.
    pub chroma: (u8, u8),
}

pub fn jpeg_info(data: &[u8]) -> Res<JpegInfo> {
    let d = mozjpeg::Decompress::new_mem(data).map_err(err)?;
    let c = d.components();
    let factor = |max: c_int, own: c_int| (max / own.max(1)).clamp(1, 4) as u8;
    let chroma = match (c.first(), c.get(1)) {
        (Some(y), Some(cb)) => {
            (factor(y.h_samp_factor, cb.h_samp_factor), factor(y.v_samp_factor, cb.v_samp_factor))
        }
        _ => (1, 1),
    };
    Ok(JpegInfo { w: d.width(), h: d.height(), components: c.len(), gray: c.len() == 1, chroma })
}

pub fn jpeg_decode(data: &[u8]) -> Res<Raster> {
    let mut d = mozjpeg::Decompress::new_mem(data).map_err(err)?.rgb().map_err(err)?;
    let (w, h) = (d.width(), d.height());
    let data = d.read_scanlines::<u8>().map_err(err)?;
    d.finish().map_err(err)?;
    Ok(Raster { w, h, ch: 3, data })
}

fn jpeg_decode_cmyk(data: &[u8]) -> Res<Vec<u8>> {
    let d = mozjpeg::Decompress::new_mem(data).map_err(err)?;
    let mut d = d.to_colorspace(mozjpeg::ColorSpace::JCS_CMYK).map_err(err)?;
    let data = d.read_scanlines::<u8>().map_err(err)?;
    d.finish().map_err(err)?;
    Ok(data)
}

pub fn jpeg_encode(r: &Raster, info: &JpegInfo, quality: u8, last: bool) -> Res<Vec<u8>> {
    use mozjpeg::{ColorSpace, Compress};
    let gray: Vec<u8>;
    let (mut c, pixels) = if info.gray {
        gray = r.data.chunks_exact(3).map(|p| p[0]).collect();
        (Compress::new(ColorSpace::JCS_GRAYSCALE), &gray)
    } else {
        (Compress::new(ColorSpace::JCS_RGB), &r.data)
    };
    c.set_size(r.w, r.h);
    if info.gray {
        c.set_color_space(ColorSpace::JCS_GRAYSCALE);
    }
    c.set_quality(f32::from(quality));
    c.set_progressive_mode();
    c.set_optimize_coding(true);
    c.set_optimize_scans(last);
    if !info.gray {
        c.set_chroma_sampling_pixel_sizes(info.chroma, info.chroma);
    }
    let mut c = c.start_compress(Vec::new()).map_err(err)?;
    c.write_scanlines(pixels).map_err(err)?;
    c.finish().map_err(err)
}

struct Transcoder {
    src: ffi::jpeg_decompress_struct,
    dst: ffi::jpeg_compress_struct,
    src_err: ffi::jpeg_error_mgr,
    dst_err: ffi::jpeg_error_mgr,
    out: *mut u8,
    out_len: c_ulong,
}

impl Drop for Transcoder {
    fn drop(&mut self) {
        // SAFETY: both structs were created in `jpeg_transcode`; `out` is malloc'd by libjpeg or null.
        unsafe {
            ffi::jpeg_destroy_compress(&mut self.dst);
            ffi::jpeg_destroy_decompress(&mut self.src);
            libc::free(self.out.cast());
        }
    }
}

extern "C-unwind" fn on_error(_: &mut ffi::jpeg_common_struct) {
    // `resume_unwind` skips the panic hook; `jpeg_transcode` catches it.
    std::panic::resume_unwind(Box::new("libjpeg"));
}

extern "C-unwind" fn on_message(cinfo: &mut ffi::jpeg_common_struct, level: c_int) {
    if level < 0 {
        // SAFETY: libjpeg always sets `err` before emitting messages.
        unsafe { (*cinfo.err).num_warnings += 1 };
    }
}

/// Re-packs the existing DCT coefficients (like `jpegtran`): every decoded pixel stays the same.
pub fn jpeg_transcode(data: &[u8]) -> Res<Vec<u8>> {
    let run = || -> Res<Vec<u8>> {
        // SAFETY: plain C structs; zeroed is the documented initial state. The Box keeps the
        // addresses stable because libjpeg stores pointers into it.
        unsafe {
            let mut t: Box<Transcoder> = Box::new(std::mem::zeroed());
            let t = &mut *t;
            for e in [&mut t.src_err, &mut t.dst_err] {
                ffi::jpeg_std_error(e);
                e.error_exit = Some(on_error);
                e.emit_message = Some(on_message);
            }
            t.src.common.err = &mut t.src_err;
            t.dst.common.err = &mut t.dst_err;
            ffi::jpeg_create_decompress(&mut t.src);
            ffi::jpeg_create_compress(&mut t.dst);
            ffi::jpeg_mem_src(&mut t.src, data.as_ptr(), data.len() as c_ulong);
            ffi::jpeg_read_header(&mut t.src, 1);
            let coefficients = ffi::jpeg_read_coefficients(&mut t.src);
            ffi::jpeg_copy_critical_parameters(&t.src, &mut t.dst);
            t.dst.optimize_coding = 1;
            ffi::jpeg_simple_progression(&mut t.dst);
            ffi::jpeg_mem_dest(&mut t.dst, &mut t.out, &mut t.out_len);
            ffi::jpeg_write_coefficients(&mut t.dst, coefficients);
            ffi::jpeg_finish_compress(&mut t.dst);
            ffi::jpeg_finish_decompress(&mut t.src);
            if t.src_err.num_warnings > 0 {
                return Err("Dữ liệu JPG bị thiếu hoặc hỏng".into());
            }
            Ok(std::slice::from_raw_parts(t.out, t.out_len as usize).to_vec())
        }
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err("Không đọc được file JPG".into()))
}

pub fn jpeg_same_pixels(a: &[u8], b: &[u8], components: usize) -> Res<bool> {
    if components == 4 {
        return Ok(jpeg_decode_cmyk(a)? == jpeg_decode_cmyk(b)?);
    }
    Ok(jpeg_decode(a)?.data == jpeg_decode(b)?.data)
}

// ---------------------------------------------------------------- PNG

pub fn png_decode(data: &[u8]) -> Res<image::DynamicImage> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(data), image::ImageFormat::Png);
    reader.no_limits();
    reader.decode().map_err(err)
}

fn png_options() -> oxipng::Options {
    let mut o = oxipng::Options::from_preset(4);
    o.optimize_alpha = false;
    o.interlace = Some(false);
    o
}

pub fn png_optimize(data: &[u8], strip: bool) -> Res<Vec<u8>> {
    let mut o = png_options();
    o.strip = if strip {
        oxipng::StripChunks::Keep(container::PNG_DISPLAY.into_iter().collect())
    } else {
        oxipng::StripChunks::None
    };
    oxipng::optimize_from_memory(data, &o).map_err(err)
}

/// Quick variant for predictions.
pub fn png_optimize_fast(data: &[u8]) -> Res<Vec<u8>> {
    let mut o = oxipng::Options::from_preset(1);
    o.optimize_alpha = false;
    o.interlace = Some(false);
    oxipng::optimize_from_memory(data, &o).map_err(err)
}

/// Quick variant for predictions, without metadata.
pub fn png_encode_fast(r: &Raster) -> Res<Vec<u8>> {
    let color = if r.ch == 4 {
        oxipng::ColorType::RGBA
    } else {
        oxipng::ColorType::RGB { transparent_color: None }
    };
    oxipng::RawImage::new(r.w as u32, r.h as u32, color, oxipng::BitDepth::Eight, r.data.clone())
        .and_then(|raw| raw.create_optimized_png(&oxipng::Options::from_preset(1)))
        .map_err(err)
}

pub fn png_same_pixels(a: &[u8], b: &[u8]) -> Res<bool> {
    let (a, b) = (png_decode(a)?, png_decode(b)?);
    if a.color().bytes_per_pixel() / a.color().channel_count() > 1 {
        return Ok(a.to_rgba16() == b.to_rgba16());
    }
    Ok(a.to_rgba8() == b.to_rgba8())
}

/// Reduces the picture to at most 256 colours.
pub fn quantize(r: &Raster) -> Res<Raster> {
    if r.ch == 3 {
        let img = image::RgbImage::from_raw(r.w as u32, r.h as u32, r.data.clone())
            .ok_or("Kích thước ảnh không khớp")?;
        let img = quantette::ImageBuf::try_from(img).map_err(err)?;
        let out = quantette::Pipeline::new()
            .quantize_method(quantette::QuantizeMethod::kmeans())
            .parallel(true)
            .input_image(img.as_ref())
            .output_srgb8_image();
        let data = out.as_ref().as_slice().iter().flat_map(|c| [c.red, c.green, c.blue]).collect();
        return Ok(Raster { data, ..*r });
    }
    let mut data = r.data.clone();
    for p in data.chunks_exact_mut(4).filter(|p| p[3] == 0) {
        p.fill(0);
    }
    let nq = color_quant::NeuQuant::new(3, 256, &data);
    for p in data.chunks_exact_mut(4) {
        nq.map_pixel(p);
    }
    Ok(Raster { data, ..*r })
}

pub fn png_encode(r: &Raster, source: &[u8], icc: Option<&[u8]>, strip: bool) -> Res<Vec<u8>> {
    let color = if r.ch == 4 {
        oxipng::ColorType::RGBA
    } else {
        oxipng::ColorType::RGB { transparent_color: None }
    };
    let (w, h) = (r.w as u32, r.h as u32);
    let mut raw = oxipng::RawImage::new(w, h, color, oxipng::BitDepth::Eight, r.data.clone())
        .map_err(err)?;
    if let Some(icc) = icc {
        raw.add_icc_profile(icc);
    }
    for (name, body) in container::png_chunks(source) {
        if container::png_survives_requantise(&name)
            && (!strip || container::PNG_DISPLAY.contains(&name))
        {
            raw.add_png_chunk(name, body.to_vec());
        }
    }
    raw.create_optimized_png(&png_options()).map_err(err)
}

// ---------------------------------------------------------------- WebP

pub fn webp_size(data: &[u8]) -> Option<(usize, usize)> {
    let f = webp::BitstreamFeatures::new(data)?;
    Some((f.width() as usize, f.height() as usize))
}

pub fn webp_decode(data: &[u8]) -> Res<Raster> {
    let img = webp::Decoder::new(data).decode().ok_or("Không đọc được file WebP")?;
    let ch = if img.is_alpha() { 4 } else { 3 };
    Ok(Raster { w: img.width() as usize, h: img.height() as usize, ch, data: img.to_vec() })
}

pub fn webp_encode(r: &Raster, quality: Option<u8>, last: bool) -> Res<Vec<u8>> {
    webp_encode_with(r, quality, if last { 6 } else { 4 })
}

/// Quick lossless variant for predictions.
pub fn webp_lossless_fast(r: &Raster) -> Res<Vec<u8>> {
    webp_encode_with(r, None, 1)
}

fn webp_encode_with(r: &Raster, quality: Option<u8>, method: i32) -> Res<Vec<u8>> {
    let mut c = webp::WebPConfig::new().map_err(|()| "cấu hình WebP không hợp lệ")?;
    c.lossless = i32::from(quality.is_none());
    c.quality = quality.map_or(75.0, f32::from);
    c.method = method;
    c.exact = 1;
    c.alpha_quality = 100;
    c.use_sharp_yuv = 1;
    c.thread_level = 1;
    let (w, h) = (r.w as u32, r.h as u32);
    let e = if r.ch == 4 {
        webp::Encoder::from_rgba(&r.data, w, h)
    } else {
        webp::Encoder::from_rgb(&r.data, w, h)
    };
    Ok(e.encode_advanced(&c).map_err(|e| format!("{e:?}"))?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upright_matches_the_exif_definition() {
        // 3x2: rows "012" / "345"
        let r = Raster { w: 3, h: 2, ch: 3, data: (0..6).flat_map(|v| [v, v, v]).collect() };
        let run = |o| r.clone().upright(o).data.chunks(3).map(|p| p[0]).collect::<Vec<_>>();
        assert_eq!(run(1), [0, 1, 2, 3, 4, 5]);
        assert_eq!(run(2), [2, 1, 0, 5, 4, 3]);
        assert_eq!(run(3), [5, 4, 3, 2, 1, 0]);
        assert_eq!(run(4), [3, 4, 5, 0, 1, 2]);
        assert_eq!(run(5), [0, 3, 1, 4, 2, 5]);
        assert_eq!(run(6), [3, 0, 4, 1, 5, 2]);
        assert_eq!(run(7), [5, 2, 4, 1, 3, 0]);
        assert_eq!(run(8), [2, 5, 1, 4, 0, 3]);
    }
}
