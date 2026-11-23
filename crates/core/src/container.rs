//! Minimal readers/writers for the metadata containers of JPEG, PNG and WebP.

/// Byte offset, endianness and value of the orientation tag in a raw TIFF block.
fn orientation_slot(tiff: &[u8]) -> Option<(usize, bool, u16)> {
    let le = match tiff.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let b: [u8; 4] = tiff.get(4..8)?.try_into().ok()?;
    let ifd = if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) } as usize;
    let count = u16_at(ifd)? as usize;
    for i in 0..count {
        let entry = ifd.checked_add(2 + i * 12)?;
        if u16_at(entry)? == 0x0112 {
            let offset = entry + 8;
            return Some((offset, le, u16_at(offset)?));
        }
    }
    None
}

pub fn orientation(tiff: &[u8]) -> u8 {
    match orientation_slot(tiff) {
        Some((_, _, v @ 1..=8)) => v as u8,
        _ => 1,
    }
}

/// Marks the EXIF block as upright, for use after rotating the pixels themselves.
pub fn reset_orientation(tiff: &mut [u8]) {
    if let Some((offset, le, _)) = orientation_slot(tiff) {
        let v = if le { 1u16.to_le_bytes() } else { 1u16.to_be_bytes() };
        tiff[offset..offset + 2].copy_from_slice(&v);
    }
}

/// An EXIF block that carries nothing except the orientation.
pub fn minimal_exif(orientation: u8) -> Vec<u8> {
    let mut v = vec![b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0];
    v.push(orientation);
    v.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    v
}

const EXIF_ID: &[u8] = b"Exif\0\0";
const ICC_ID: &[u8] = b"ICC_PROFILE\0";
const XMP_ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

#[derive(Default, Clone)]
pub struct Metadata {
    /// Raw TIFF block, without the JPEG `Exif\0\0` prefix.
    pub exif: Option<Vec<u8>>,
    pub icc: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
}

impl Metadata {
    pub fn orientation(&self) -> u8 {
        self.exif.as_deref().map_or(1, orientation)
    }

    /// Drops everything personal; keeps what is needed to display the picture correctly.
    pub fn stripped(&self) -> Self {
        let o = self.orientation();
        Self { exif: (o != 1).then(|| minimal_exif(o)), icc: self.icc.clone(), xmp: None }
    }

    pub fn is_srgb(&self) -> bool {
        self.icc.as_deref().is_none_or(|icc| {
            contains(icc, b"sRGB") || contains(icc, &[0, b's', 0, b'R', 0, b'G', 0, b'B'])
        })
    }
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---------------------------------------------------------------- JPEG

/// `(marker, payload)` of every segment before the first scan.
fn jpeg_segments(data: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    if data.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut out = Vec::new();
    let mut pos = 2;
    loop {
        if *data.get(pos)? != 0xFF {
            return None;
        }
        while *data.get(pos)? == 0xFF {
            pos += 1;
        }
        let marker = data[pos];
        pos += 1;
        if marker == 0xDA {
            return Some(out);
        }
        let len = u16::from_be_bytes(data.get(pos..pos + 2)?.try_into().ok()?) as usize;
        out.push((marker, data.get(pos + 2..pos.checked_add(len)?)?));
        pos += len;
    }
}

fn is_metadata_marker(marker: u8) -> bool {
    (0xE0..=0xEF).contains(&marker) || marker == 0xFE
}

pub fn jpeg_metadata(data: &[u8]) -> Metadata {
    let mut m = Metadata::default();
    let mut icc: Vec<(u8, &[u8])> = Vec::new();
    for (marker, p) in jpeg_segments(data).unwrap_or_default() {
        match marker {
            0xE1 if p.starts_with(EXIF_ID) && m.exif.is_none() => m.exif = Some(p[6..].to_vec()),
            0xE1 if p.starts_with(XMP_ID) && m.xmp.is_none() => {
                m.xmp = Some(p[XMP_ID.len()..].to_vec());
            }
            0xE2 if p.starts_with(ICC_ID) && p.len() > 14 => icc.push((p[12], &p[14..])),
            _ => {}
        }
    }
    if !icc.is_empty() {
        icc.sort_by_key(|(n, _)| *n);
        m.icc = Some(icc.into_iter().flat_map(|(_, d)| d.iter().copied()).collect());
    }
    m
}

/// Pictures that carry a second payload (HDR gain map, embedded video) a re-encode would lose.
pub fn jpeg_has_companion(data: &[u8]) -> bool {
    const NEEDLES: [&[u8]; 6] = [
        b"hdrgm:",
        b"HDRGainMap",
        b"MotionPhoto",
        b"MicroVideo",
        b"urn:iso:std:iso:ts:21496",
        b"GContainer",
    ];
    data.ends_with(b"SEFT")
        || jpeg_segments(data)
            .unwrap_or_default()
            .iter()
            .any(|(m, p)| is_metadata_marker(*m) && NEEDLES.iter().any(|n| contains(p, n)))
}

/// Rebuilds `encoded` with the metadata segments of `source`.
pub fn jpeg_with_metadata(encoded: &[u8], source: &[u8], strip: bool) -> Option<Vec<u8>> {
    fn put(out: &mut Vec<u8>, marker: u8, p: &[u8]) {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&((p.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(p);
    }
    let mut out = vec![0xFF, 0xD8];
    for (marker, p) in jpeg_segments(encoded)? {
        if matches!(marker, 0xE0 | 0xEE) {
            put(&mut out, marker, p);
        }
    }
    if strip && let Some(exif) = jpeg_metadata(source).stripped().exif {
        put(&mut out, 0xE1, &[EXIF_ID, &exif].concat());
    }
    for (marker, p) in jpeg_segments(source)? {
        let keep = match marker {
            m if !is_metadata_marker(m) => false,
            0xE0 | 0xEE => false,                     // written by the encoder
            0xE2 if p.starts_with(b"MPF\0") => false, // its offsets would dangle
            0xE2 if p.starts_with(ICC_ID) => true,
            _ => !strip,
        };
        if keep {
            put(&mut out, marker, p);
        }
    }
    // Everything else the encoder produced (tables, frame header, scans).
    let mut pos = 2;
    while is_metadata_marker(*encoded.get(pos + 1)?) {
        pos += 2 + u16::from_be_bytes(encoded.get(pos + 2..pos + 4)?.try_into().ok()?) as usize;
    }
    out.extend_from_slice(encoded.get(pos..)?);
    Some(out)
}

// ---------------------------------------------------------------- PNG

pub fn png_chunks(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut pos = 8;
    while let Some(head) = data.get(pos..pos + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let Some(body) = data.get(pos + 8..).and_then(|d| d.get(..len)) else { break };
        out.push(([head[4], head[5], head[6], head[7]], body));
        pos += 12 + len;
    }
    out
}

/// Chunks that describe how the pixels must be displayed.
pub const PNG_DISPLAY: [[u8; 4]; 7] =
    [*b"cICP", *b"iCCP", *b"sRGB", *b"gAMA", *b"cHRM", *b"pHYs", *b"mDCV"];

/// Ancillary chunks that stay valid after the colour type changes.
pub fn png_survives_requantise(name: &[u8; 4]) -> bool {
    name[0] & 0x20 != 0
        && !matches!(
            name,
            b"tRNS" | b"bKGD" | b"sBIT" | b"hIST" | b"sPLT" | b"iCCP" | b"acTL" | b"fcTL" | b"fdAT"
        )
}

// ---------------------------------------------------------------- WebP

#[derive(Default)]
pub struct WebpInfo {
    pub animated: bool,
    pub lossless: bool,
    pub meta: Metadata,
}

pub fn webp_chunks(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return out;
    }
    let mut pos = 12;
    while let Some(head) = data.get(pos..pos + 8) {
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let Some(body) = data.get(pos + 8..).and_then(|d| d.get(..len)) else { break };
        out.push(([head[0], head[1], head[2], head[3]], body));
        pos += 8 + len + (len & 1);
    }
    out
}

pub fn webp_info(data: &[u8]) -> WebpInfo {
    let mut info = WebpInfo::default();
    for (name, body) in webp_chunks(data) {
        match &name {
            b"ANIM" | b"ANMF" => info.animated = true,
            b"VP8L" => info.lossless = true,
            b"ICCP" => info.meta.icc = Some(body.to_vec()),
            b"XMP " => info.meta.xmp = Some(body.to_vec()),
            b"EXIF" => info.meta.exif = Some(body.strip_prefix(EXIF_ID).unwrap_or(body).to_vec()),
            _ => {}
        }
    }
    info
}

/// Wraps the bitstream of a still WebP with the given metadata.
pub fn webp_with_metadata(encoded: &[u8], w: u32, h: u32, alpha: bool, meta: &Metadata) -> Vec<u8> {
    fn put(body: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
        body.extend_from_slice(name);
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(data);
        if data.len() & 1 == 1 {
            body.push(0);
        }
    }
    let image: Vec<_> = webp_chunks(encoded)
        .into_iter()
        .filter(|(n, _)| matches!(n, b"ALPH" | b"VP8 " | b"VP8L"))
        .collect();
    let mut body = b"WEBP".to_vec();
    let has_meta = meta.exif.is_some() || meta.icc.is_some() || meta.xmp.is_some();
    if has_meta || image.iter().any(|(n, _)| n == b"ALPH") {
        let flags = (u8::from(meta.icc.is_some()) << 5)
            | (u8::from(alpha) << 4)
            | (u8::from(meta.exif.is_some()) << 3)
            | (u8::from(meta.xmp.is_some()) << 2);
        let mut x = vec![flags, 0, 0, 0];
        x.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        x.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        put(&mut body, b"VP8X", &x);
    }
    if let Some(icc) = &meta.icc {
        put(&mut body, b"ICCP", icc);
    }
    for (name, data) in &image {
        put(&mut body, name, data);
    }
    if let Some(exif) = &meta.exif {
        put(&mut body, b"EXIF", exif);
    }
    if let Some(xmp) = &meta.xmp {
        put(&mut body, b"XMP ", xmp);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_exif_round_trips() {
        for o in 1..=8 {
            let mut e = minimal_exif(o);
            assert_eq!(orientation(&e), o);
            reset_orientation(&mut e);
            assert_eq!(orientation(&e), 1);
        }
    }

    #[test]
    fn garbage_never_panics() {
        for n in 0..256usize {
            let junk: Vec<u8> = (0..n).map(|i| (i * 37 + n) as u8).collect();
            orientation(&junk);
            orientation(&[b"II*\0".as_slice(), &junk].concat());
            jpeg_metadata(&[&[0xFF, 0xD8, 0xFF, 0xE1][..], &junk].concat());
            png_chunks(&junk);
            webp_info(&[b"RIFF\0\0\0\0WEBP".as_slice(), &junk].concat());
        }
    }
}
