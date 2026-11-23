use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nenanh_core::batch::{self, Item, Report};
use nenanh_core::{Level, Options, Target};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[derive(Default)]
struct Job {
    running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Serialize)]
struct Progress {
    index: usize,
    report: Report,
}

#[tauri::command(async)]
fn scan(paths: Vec<PathBuf>) -> Vec<Item> {
    batch::scan(&paths)
}

#[tauri::command]
fn start(
    app: AppHandle,
    job: State<'_, Job>,
    items: Vec<Item>,
    out_dir: Option<PathBuf>,
    options: Options,
) -> Result<(), String> {
    if job.running.swap(true, Ordering::SeqCst) {
        return Err("Đang nén, vui lòng đợi".into());
    }
    job.cancel.store(false, Ordering::SeqCst);
    let (running, cancel) = (job.running.clone(), job.cancel.clone());
    std::thread::spawn(move || {
        batch::run(&items, out_dir.as_deref(), options, &cancel, &|index, report| {
            let _ = app.emit("progress", Progress { index, report });
        });
        running.store(false, Ordering::SeqCst);
        let _ = app.emit("finished", ());
    });
    Ok(())
}

#[tauri::command]
fn cancel(job: State<'_, Job>) {
    job.cancel.store(true, Ordering::SeqCst);
}

/// `NenAnh --smoke <input> <output>`: compresses without a window, used to check an installed copy.
fn smoke(input: PathBuf, output: PathBuf) -> i32 {
    let items = batch::scan(&[input]);
    let failed = AtomicBool::new(items.is_empty());
    for target in [Target::Keep, Target::Webp, Target::Avif] {
        let options = Options { target, level: Level::Balanced, strip_metadata: false };
        batch::run(&items, Some(&output), options, &AtomicBool::new(false), &|_, report| {
            if !matches!(report, Report::Working | Report::Done { kept: false, .. }) {
                failed.store(true, Ordering::SeqCst);
            }
        });
    }
    i32::from(failed.into_inner())
}

pub fn run() {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == "--smoke")
        && let (Some(input), Some(output)) = (args.next(), args.next())
    {
        std::process::exit(smoke(input.into(), output.into()));
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Job::default())
        .invoke_handler(tauri::generate_handler![scan, start, cancel])
        .run(tauri::generate_context!())
        .expect("không khởi động được ứng dụng");
}
