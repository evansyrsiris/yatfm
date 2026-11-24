//! Slow work (copy, move, delete, archives) runs on a background thread so the UI never freezes.
//!
//! The UI starts a `Job`, then every few milliseconds calls `poll()` to hear what happened.
//! Only one job runs at a time. Ctrl-x asks it to stop.

use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::Ordering,
        mpsc::{channel, Receiver, Sender},
        Arc,
    },
    thread,
};

use crate::{
    archive,
    format::{human_size, plural},
    fsops::{self, Progress},
    undo::Undo,
};

/// What to do when pasting something whose name is already taken.
#[derive(Clone, Copy)]
pub enum OnConflict {
    /// Keep both: the new one becomes `name (1)`.
    Keep,
    /// The old one goes to the trash first.
    Overwrite,
    Skip,
}

pub enum Job {
    Copy { items: Vec<PathBuf>, dest: PathBuf, on_conflict: OnConflict },
    Move { items: Vec<PathBuf>, dest: PathBuf, on_conflict: OnConflict },
    Trash { items: Vec<PathBuf> },
    Delete { items: Vec<PathBuf> },
    Extract { archives: Vec<PathBuf>, dest: PathBuf },
    Compress { items: Vec<PathBuf>, dir: PathBuf, name: String },
}

/// What a finished job tells the UI.
pub struct Outcome {
    pub message: String,
    pub undo: Option<Undo>,
}

enum Event {
    Progress(String),
    Finished(Result<Outcome, String>),
}

pub struct Jobs {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    pub running: usize,
    /// e.g. "Copying 3/14"
    label: Option<String>,
    shared: Option<Arc<Progress>>,
}

impl Jobs {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Jobs { tx, rx, running: 0, label: None, shared: None }
    }

    pub fn start(&mut self, job: Job) {
        self.running += 1;
        let shared = Arc::new(Progress::default());
        self.shared = Some(shared.clone());

        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = run(job, &tx, &shared);
            let _ = tx.send(Event::Finished(result));
        });
    }

    /// Ask the running job to stop. It stops between files (or in the middle of a big one).
    pub fn cancel(&self) {
        if let Some(shared) = &self.shared {
            shared.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The text for the status bar while a job runs: "Copying 3/14 · 1.2 GB / 4.0 GB · 30%".
    pub fn status(&self) -> Option<String> {
        if self.running == 0 {
            return None;
        }
        let label = self.label.clone().unwrap_or_else(|| "Working...".to_string());
        let Some(shared) = &self.shared else {
            return Some(label);
        };

        let done = shared.done.load(Ordering::Relaxed);
        let total = shared.total.load(Ordering::Relaxed);
        let hint = if shared.cancelled() { "cancelling..." } else { "Ctrl-x cancels" };

        Some(if total > 0 {
            let percent = (done * 100 / total).min(100);
            format!("{label} · {} / {} · {percent}%  ({hint})", human_size(done), human_size(total))
        } else if done > 0 {
            format!("{label} · {}  ({hint})", human_size(done))
        } else {
            format!("{label}  ({hint})")
        })
    }

    /// Collect everything that happened since last time. Returns the jobs that finished.
    pub fn poll(&mut self) -> Vec<Result<Outcome, String>> {
        let mut finished = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Progress(text) => self.label = Some(text),
                Event::Finished(result) => {
                    self.running -= 1;
                    finished.push(result);
                }
            }
        }
        if self.running == 0 {
            self.label = None;
            self.shared = None;
        }
        finished
    }
}

fn run(job: Job, tx: &Sender<Event>, progress: &Progress) -> Result<Outcome, String> {
    match job {
        Job::Copy { items, dest, on_conflict } => transfer(items, dest, false, on_conflict, tx, progress),
        Job::Move { items, dest, on_conflict } => transfer(items, dest, true, on_conflict, tx, progress),

        Job::Trash { items } => {
            let _ = tx.send(Event::Progress("Moving to trash...".into()));
            trash::delete_all(&items).map_err(|e| e.to_string())?;
            Ok(Outcome {
                message: format!("Moved {} to the trash (u to undo)", plural(items.len(), "item")),
                undo: Some(Undo::Trashed(items)),
            })
        }

        Job::Delete { items } => {
            let total = items.len();
            let mut deleted = 0;
            for (i, path) in items.iter().enumerate() {
                if progress.cancelled() {
                    break;
                }
                let _ = tx.send(Event::Progress(format!("Deleting {}/{total}", i + 1)));
                fsops::remove_path(path).map_err(|e| format!("{}: {e}", path.display()))?;
                deleted += 1;
            }
            let mut message = format!("Deleted {}", plural(deleted, "item"));
            if deleted < total {
                message += " · cancelled";
            }
            Ok(Outcome { message, undo: None })
        }

        Job::Extract { archives, dest } => {
            let total = archives.len();
            let mut created = Vec::new();
            let mut errors = Vec::new();
            for (i, file) in archives.iter().enumerate() {
                if progress.cancelled() {
                    break;
                }
                let _ = tx.send(Event::Progress(format!("Extracting {}/{total}", i + 1)));
                match archive::extract(file, &dest) {
                    Ok(folder) => created.push(folder),
                    Err(e) => errors.push(e),
                }
            }
            created_outcome("Extracted", created, errors)
        }

        Job::Compress { items, dir, name } => {
            let _ = tx.send(Event::Progress(format!("Compressing to {name}...")));
            let archive_path = archive::compress(&items, &dir, &name)?;
            Ok(Outcome { message: format!("Created {name}"), undo: Some(Undo::Created(vec![archive_path])) })
        }
    }
}

fn created_outcome(word: &str, created: Vec<PathBuf>, errors: Vec<String>) -> Result<Outcome, String> {
    if created.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let mut message = format!("{word} {}", plural(created.len(), "archive"));
    if !errors.is_empty() {
        message += &format!(" ({} failed: {})", errors.len(), errors.join("; "));
    }
    let undo = if created.is_empty() { None } else { Some(Undo::Created(created)) };
    Ok(Outcome { message, undo })
}

/// Copy or move `items` into `dest`. Keeps going if one item fails, and reports at the end.
fn transfer(
    items: Vec<PathBuf>,
    dest: PathBuf,
    is_move: bool,
    on_conflict: OnConflict,
    tx: &Sender<Event>,
    progress: &Progress,
) -> Result<Outcome, String> {
    let (doing, done_word) = if is_move { ("Moving", "Moved") } else { ("Copying", "Copied") };
    let total = items.len();

    // Know the size up front so the status bar can show a percentage.
    // (Moves are usually instant renames, so we don't scan for those.)
    if !is_move {
        let bytes: u64 = items.iter().map(|path| fsops::tree_size(path)).sum();
        progress.total.store(bytes, Ordering::Relaxed);
    }

    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut skipped = 0;
    let mut replaced = 0;
    let mut cancelled = false;

    for (i, from) in items.iter().enumerate() {
        if progress.cancelled() {
            cancelled = true;
            break;
        }
        let _ = tx.send(Event::Progress(format!("{doing} {}/{total}", i + 1)));

        let Some(name) = from.file_name() else {
            continue;
        };
        let label = name.to_string_lossy();

        if dest.starts_with(from) {
            errors.push(format!("{label}: can't go inside itself"));
            continue;
        }
        if is_move && from.parent() == Some(dest.as_path()) {
            continue; // already here
        }

        let target = dest.join(name);
        let to = if !fsops::exists(&target) {
            target
        } else if target == *from {
            fsops::unique_path(&dest, name) // copying into its own folder: make a duplicate
        } else {
            match on_conflict {
                OnConflict::Keep => fsops::unique_path(&dest, name),
                OnConflict::Skip => {
                    skipped += 1;
                    continue;
                }
                OnConflict::Overwrite => match trash::delete(&target) {
                    Ok(()) => {
                        replaced += 1;
                        target
                    }
                    Err(e) => {
                        errors.push(format!("{label}: {e}"));
                        continue;
                    }
                },
            }
        };

        let result = if is_move { fsops::move_path(from, &to, progress) } else { fsops::copy_path(from, &to, progress) };
        match result {
            Ok(()) => done.push((from.clone(), to)),
            Err(_) if progress.cancelled() => {
                let _ = fsops::remove_path(&to); // half-copied leftovers
                cancelled = true;
                break;
            }
            Err(e) => {
                if e.kind() != io::ErrorKind::AlreadyExists {
                    let _ = fsops::remove_path(&to);
                }
                errors.push(format!("{label}: {e}"));
            }
        }
    }

    if done.is_empty() && !errors.is_empty() && !cancelled {
        return Err(errors.join("; "));
    }

    let mut message = format!("{done_word} {}", plural(done.len(), "item"));
    if replaced > 0 {
        message += &format!(", replaced {replaced} (the old ones are in the trash)");
    }
    if skipped > 0 {
        message += &format!(", skipped {skipped}");
    }
    if cancelled {
        message += " · cancelled";
    }
    if !errors.is_empty() {
        message += &format!(" ({} failed: {})", errors.len(), errors.join("; "));
    }

    let undo = match (done.is_empty(), is_move) {
        (true, _) => None,
        (false, true) => Some(Undo::Moved(done)),
        (false, false) => Some(Undo::Created(done.into_iter().map(|(_, to)| to).collect())),
    };
    Ok(Outcome { message, undo })
}
