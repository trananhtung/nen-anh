//! SSIMULACRA2 of PNG pairs: `score a.png b.png [a.png b.png ...]`
use fast_ssim2::{PixelDescriptor, PixelSlice, compute_ssimulacra2};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    for p in a.chunks(2) {
        let load = |f: &str| image::open(f).unwrap().into_rgb8();
        let (x, y) = (load(&p[0]), load(&p[1]));
        let (w, h) = x.dimensions();
        let s = |i: &image::RgbImage| compute_ssimulacra2(
            &PixelSlice::new(x.as_raw(), w, h, w as usize * 3, PixelDescriptor::RGB8_SRGB).unwrap(),
            &PixelSlice::new(i.as_raw(), w, h, w as usize * 3, PixelDescriptor::RGB8_SRGB).unwrap()).unwrap();
        println!("{:6.1} {}", s(&y), p[1].rsplit('/').next().unwrap());
    }
}
