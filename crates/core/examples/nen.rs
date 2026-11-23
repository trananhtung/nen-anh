//! `cargo run --release --example nen -- <keep|webp|avif> <lossless|high|balanced|small> <out> <paths...>`
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use nenanh_core::{Options, batch};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let parse = |s: &str| format!("\"{s}\"");
    let opts = Options {
        target: serde_json::from_str(&parse(&a[0])).expect("target"),
        level: serde_json::from_str(&parse(&a[1])).expect("level"),
        strip_metadata: false,
    };
    let items = batch::scan(&a[3..].iter().map(PathBuf::from).collect::<Vec<_>>());
    let t = Instant::now();
    batch::run(&items, Some(Path::new(&a[2])), opts, &AtomicBool::new(false), &|i, r| match r {
        batch::Report::Done { before, after, score, lossless, kept, note, .. } => println!(
            "{:>6.1}s {:>9} -> {:>9} ({:>3.0}%) score {:?} lossless={lossless} kept={kept} {} {}",
            t.elapsed().as_secs_f32(), before, after,
            100.0 - after as f64 / before as f64 * 100.0,
            score.map(|s| s.round()), note.unwrap_or_default(),
            items[i].path.file_name().unwrap().to_string_lossy()
        ),
        batch::Report::Failed { message } => println!("FAILED {} {message}", items[i].path.display()),
        _ => {}
    });
}
