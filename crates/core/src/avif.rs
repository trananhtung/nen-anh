//! Still-picture AVIF built on rav1e, returning the encoder's own reconstruction so that the
//! result can be scored without a separate decoder.

use rav1e::prelude::*;

use crate::codec::Raster;
use crate::container::Metadata;

type Res<T> = Result<T, String>;

const SPEED: u8 = 7;

fn quantizer(quality: u8) -> usize {
    let q = f32::from(quality) / 100.0;
    let x = if q >= 0.82 { (1.0 - q) * 2.6 } else { q.mul_add(-0.5, 0.875) };
    (x * 255.0).round().max(1.0) as usize
}

fn config(w: usize, h: usize, quantizer: usize, color: bool) -> Config {
    let mut speed = SpeedSettings::from_preset(SPEED);
    speed.multiref = false;
    speed.rdo_lookahead_frames = 1;
    speed.scene_detection_mode = SceneDetectionSpeed::None;
    let tiles = std::thread::available_parallelism().map_or(1, |n| n.get()).min(w * h / (256 * 256));
    Config::new().with_encoder_config(EncoderConfig {
        width: w,
        height: h,
        bit_depth: 10,
        chroma_sampling: if color { ChromaSampling::Cs444 } else { ChromaSampling::Cs400 },
        pixel_range: PixelRange::Full,
        color_description: color.then_some(ColorDescription {
            color_primaries: ColorPrimaries::BT709,
            transfer_characteristics: TransferCharacteristics::SRGB,
            matrix_coefficients: MatrixCoefficients::BT601,
        }),
        still_picture: true,
        min_key_frame_interval: 0,
        max_key_frame_interval: 0,
        low_latency: false,
        quantizer,
        min_quantizer: quantizer as u8,
        bitrate: 0,
        tune: Tune::Psychovisual,
        tiles,
        speed_settings: speed,
        ..Default::default()
    })
}

type Planes = Vec<Vec<u16>>;

/// Encodes the planes and returns the bitstream with the planes a decoder will produce.
fn encode_av1(w: usize, h: usize, quantizer: usize, planes: &[Vec<u16>]) -> Res<(Vec<u8>, Planes)> {
    let err = |e: &dyn std::fmt::Display| format!("AVIF: {e}");
    let mut ctx: Context<u16> =
        config(w, h, quantizer, planes.len() == 3).new_context().map_err(|e| err(&e))?;
    let mut frame = ctx.new_frame();
    for (dst, src) in frame.planes.iter_mut().zip(planes) {
        let mut dst = dst.mut_slice(Default::default());
        for (d, s) in dst.rows_iter_mut().zip(src.chunks_exact(w)) {
            d[..w].copy_from_slice(s);
        }
    }
    ctx.send_frame(frame).map_err(|e| err(&e))?;
    ctx.flush();
    let (mut data, mut rec) = (Vec::new(), None);
    loop {
        match ctx.receive_packet() {
            Ok(packet) if packet.frame_type == FrameType::KEY => {
                data.extend_from_slice(&packet.data);
                rec = packet.rec.or(rec);
            }
            Ok(_) | Err(EncoderStatus::Encoded) => {}
            Err(EncoderStatus::LimitReached) => break,
            Err(e) => return Err(err(&e)),
        }
    }
    let rec = rec.ok_or("AVIF: thiếu ảnh kiểm tra")?;
    let out = (0..planes.len())
        .map(|i| {
            let p = rec.planes[i].slice(Default::default());
            p.rows_iter().take(h).flat_map(|row| row[..w].iter().copied()).collect::<Vec<u16>>()
        })
        .collect::<Planes>();
    if out.iter().any(|p| p.len() != w * h) {
        return Err("AVIF: ảnh kiểm tra sai kích thước".into());
    }
    Ok((data, out))
}

fn to_ten(x: u8) -> u16 {
    (u16::from(x) << 2) | (u16::from(x) >> 6)
}

fn to_planes(r: &Raster) -> Planes {
    let n = r.w * r.h;
    let mut out: Planes = (0..r.ch.min(3)).map(|_| Vec::with_capacity(n)).collect();
    const K: f32 = 1023.0 / 255.0;
    for p in r.data.chunks_exact(r.ch) {
        let [red, green, blue] = [p[0], p[1], p[2]].map(|v| f32::from(v) * K);
        let y = 0.299 * red + 0.587 * green + 0.114 * blue;
        out[0].push(y.round() as u16);
        out[1].push(((blue - y) / 1.772 + 512.0).round().clamp(0.0, 1023.0) as u16);
        out[2].push(((red - y) / 1.402 + 512.0).round().clamp(0.0, 1023.0) as u16);
    }
    out
}

fn from_planes(r: &Raster, color: &Planes, alpha: Option<&Planes>) -> Raster {
    const K: f32 = 255.0 / 1023.0;
    let to8 = |v: f32| (v * K).round().clamp(0.0, 255.0) as u8;
    let mut data = Vec::with_capacity(r.data.len());
    for i in 0..r.w * r.h {
        let y = f32::from(color[0][i]);
        let cb = f32::from(color[1][i]) - 512.0;
        let cr = f32::from(color[2][i]) - 512.0;
        data.push(to8(y + 1.402 * cr));
        data.push(to8(y - 0.344_136 * cb - 0.714_136 * cr));
        data.push(to8(y + 1.772 * cb));
        if let Some(a) = alpha {
            data.push(to8(f32::from(a[0][i])));
        }
    }
    Raster { w: r.w, h: r.h, ch: r.ch, data }
}

/// Returns the file and the picture it decodes to.
pub fn encode(r: &Raster, quality: u8, meta: &Metadata) -> Res<(Vec<u8>, Raster)> {
    let (color, rec) = encode_av1(r.w, r.h, quantizer(quality), &to_planes(r))?;
    let alpha = if r.ch == 4 {
        let plane = r.data.chunks_exact(4).map(|p| to_ten(p[3])).collect();
        Some(encode_av1(r.w, r.h, quantizer(quality.max(94)), &[plane])?)
    } else {
        None
    };
    let mut file = avif_serialize::Aviffy::new();
    file.matrix_coefficients(avif_serialize::constants::MatrixCoefficients::Bt601);
    if let Some(exif) = &meta.exif {
        file.set_exif(exif.clone());
    }
    let alpha_data = alpha.as_ref().map(|(d, _)| d.as_slice());
    let data = file.to_vec(&color, alpha_data, r.w as u32, r.h as u32, 10);
    Ok((data, from_planes(r, &rec, alpha.as_ref().map(|(_, p)| p))))
}
