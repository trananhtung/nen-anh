//! Finds pictures on disk and processes them in parallel within a memory budget.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use serde::{Deserialize, Serialize};

use crate::{Error, Options, compress, pixel_count};

/// Folder created next to the originals when no output folder was chosen.
pub const DEFAULT_OUTPUT: &str = "anh-nen";
const EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "jfif", "png", "webp"];
const MAX_FILE_BYTES: u64 = 1 << 30;
/// Pixels that may be in flight at once across all workers.
const PIXEL_BUDGET: usize = 48_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub path: PathBuf,
    /// Folder of the file relative to what the user dropped; recreated in the output.
    pub subdir: PathBuf,
    /// Folder in which the default output folder is created.
    pub base: PathBuf,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum Report {
    Working,
    #[serde(rename_all = "camelCase")]
    Done {
        output: PathBuf,
        before: u64,
        after: u64,
        lossless: bool,
        /// The original bytes were copied because nothing smaller was acceptable.
        kept: bool,
        score: Option<f64>,
        note: Option<String>,
    },
    Failed {
        message: String,
    },
    Cancelled,
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Expands files and folders into a list of pictures, skipping previous output folders.
pub fn scan(paths: &[PathBuf]) -> Vec<Item> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut add = |path: PathBuf, subdir: PathBuf, base: PathBuf| {
        let Ok(meta) = fs::metadata(&path) else { return };
        if meta.is_file() && seen.insert(path.clone()) {
            items.push(Item { path, subdir, base, size: meta.len() });
        }
    };
    for root in paths {
        if root.is_dir() {
            let walker = walkdir::WalkDir::new(root).sort_by_file_name().into_iter();
            for entry in walker
                .filter_entry(|e| !(e.depth() > 0 && e.file_type().is_dir() && e.file_name() == DEFAULT_OUTPUT))
                .flatten()
            {
                if entry.file_type().is_file() && is_image(entry.path()) {
                    let parent = entry.path().parent().unwrap_or(root);
                    let subdir = parent.strip_prefix(root).unwrap_or(Path::new("")).to_path_buf();
                    add(entry.path().to_path_buf(), subdir, root.clone());
                }
            }
        } else if is_image(root) {
            let base = root.parent().unwrap_or(Path::new(".")).to_path_buf();
            add(root.clone(), PathBuf::new(), base);
        }
    }
    items
}

/// Writes to a temporary file first and never replaces an existing one.
fn write_new(dir: &Path, stem: &str, ext: &str, data: &[u8]) -> Result<PathBuf, Error> {
    let io = |e: std::io::Error| Error::Failed(format!("Không ghi được file: {e}"));
    fs::create_dir_all(dir).map_err(io)?;
    let mut tmp = tempfile::Builder::new().prefix(".nenanh-").tempfile_in(dir).map_err(io)?;
    tmp.write_all(data).map_err(io)?;
    tmp.as_file().sync_all().map_err(io)?;
    for n in 1..10_000 {
        let name = if n == 1 { format!("{stem}.{ext}") } else { format!("{stem} ({n}).{ext}") };
        let target = dir.join(name);
        match tmp.persist_noclobber(&target) {
            Ok(_) => return Ok(target),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => tmp = e.file,
            Err(e) => return Err(io(e.error)),
        }
    }
    Err("Có quá nhiều file trùng tên".into())
}

pub fn process(
    item: &Item,
    out_dir: Option<&Path>,
    opts: Options,
    cancel: &AtomicBool,
    budget: &Budget,
) -> Result<Report, Error> {
    let read_err = |e: std::io::Error| Error::Failed(format!("Không đọc được file: {e}"));
    if fs::metadata(&item.path).map_err(read_err)?.len() > MAX_FILE_BYTES {
        return Err("File lớn hơn 1 GB".into());
    }
    let input = fs::read(&item.path).map_err(read_err)?;
    let _permit = budget.acquire(pixel_count(&input)?);
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let out = compress(&input, opts, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let stem = item.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let own_ext = item.path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = out.ext.map_or(own_ext, str::to_owned);
    let root = out_dir.map_or_else(|| item.base.join(DEFAULT_OUTPUT), Path::to_path_buf);
    let data = out.data.as_deref().unwrap_or(&input);
    let output = write_new(&root.join(&item.subdir), &stem, &ext, data)?;
    Ok(Report::Done {
        output,
        before: input.len() as u64,
        after: data.len() as u64,
        lossless: out.lossless,
        kept: out.data.is_none(),
        score: out.score,
        note: out.note.map(|n| if out.metadata_left { format!("{n} (chưa xoá được thông tin cá nhân)") } else { n.to_owned() }),
    })
}

/// Counting semaphore measured in pixels, so a few huge pictures cannot exhaust the memory.
#[derive(Default)]
pub struct Budget {
    used: Mutex<usize>,
    freed: Condvar,
}

pub struct Permit<'a> {
    budget: &'a Budget,
    amount: usize,
}

impl Budget {
    fn acquire(&self, pixels: usize) -> Permit<'_> {
        let amount = pixels.min(PIXEL_BUDGET);
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        while *used + amount > PIXEL_BUDGET {
            used = self.freed.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used += amount;
        Permit { budget: self, amount }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.budget.used.lock().unwrap_or_else(|e| e.into_inner()) -= self.amount;
        self.budget.freed.notify_all();
    }
}

/// Runs the whole list; `report` is called from worker threads with the item index.
pub fn run(
    items: &[Item],
    out_dir: Option<&Path>,
    opts: Options,
    cancel: &AtomicBool,
    report: &(dyn Fn(usize, Report) + Sync),
) {
    let next = AtomicUsize::new(0);
    let budget = Budget::default();
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let workers = cores.div_ceil(2).clamp(1, 8).min(items.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    if cancel.load(Ordering::Relaxed) {
                        report(i, Report::Cancelled);
                        continue;
                    }
                    report(i, Report::Working);
                    // One damaged file must not take the batch down.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        process(item, out_dir, opts, cancel, &budget)
                    }));
                    report(
                        i,
                        match result {
                            Ok(Ok(done)) => done,
                            Ok(Err(Error::Cancelled)) => Report::Cancelled,
                            Ok(Err(Error::Failed(message))) => Report::Failed { message },
                            Err(_) => Report::Failed { message: "Không xử lý được ảnh này".into() },
                        },
                    );
                }
            });
        }
    });
}
