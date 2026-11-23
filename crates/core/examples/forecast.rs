//! Predicted against real results: `forecast <keep|webp|avif> <paths...>`, prints percent saved.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use nenanh_core::{Level, Options, Target, batch, compress, estimate::estimate};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let target: Target = serde_json::from_str(&format!("\"{}\"", a[0])).expect("target");
    let items = batch::scan(&a[1..].iter().map(PathBuf::from).collect::<Vec<_>>());
    let no = AtomicBool::new(false);
    for item in &items {
        let input = std::fs::read(&item.path).unwrap();
        let n = input.len() as f64;
        let saved = |v: f64| ((1.0 - v / n) * 100.0).round() as i32;
        let t = Instant::now();
        let guess = estimate(&input, target, &no).unwrap().map(saved);
        let took = t.elapsed().as_secs_f32();
        let t = Instant::now();
        let real = [Level::Lossless, Level::High, Level::Balanced, Level::Small].map(|level| {
            let out = compress(&input, Options { target, level, strip_metadata: false }, &no).unwrap();
            saved(out.data.map_or(n, |d| d.len() as f64))
        });
        println!("{target:?}\t{guess:?}\t{real:?}\t{took:.1}s/{:.1}s\t{}", t.elapsed().as_secs_f32(), item.path.file_name().unwrap().to_string_lossy());
    }
}
