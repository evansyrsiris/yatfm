//! Plain filesystem helpers: copy, move, remove, and picking names that don't clash.

use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Linux error code for "can't rename across drives".
const EXDEV: i32 = 18;

/// Shared between the UI and a background job: how far along it is, and whether to stop.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Other, "cancelled")
}

/// True if *something* is at this path, even a broken symlink.
pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// `dir/name`, or `dir/name (1)`, `dir/name (2)`... if that is taken.
pub fn unique_path(dir: &Path, name: &OsStr) -> PathBuf {
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }

    let original = Path::new(name);
    let stem = original.file_stem().unwrap_or(name).to_string_lossy();
    let extension = original.extension().map(|e| format!(".{}", e.to_string_lossy()));

    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){}", extension.as_deref().unwrap_or(""))))
        .find(|candidate| !exists(candidate))
        .expect("an unused name always exists")
}

/// Total size in bytes of a file or everything inside a folder. Never follows symlinks.
pub fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        match fs::read_dir(path) {
            Ok(read) => read.flatten().map(|item| tree_size(&item.path())).sum::<u64>(),
            Err(_) => 0,
        }
    } else if meta.is_file() {
        meta.len()
    } else {
        0
    }
}

/// Copy a file, symlink or whole folder. Keeps permissions and modification times.
/// Stops with an error as soon as `progress` is cancelled.
pub fn copy_path(from: &Path, to: &Path, progress: &Progress) -> io::Result<()> {
    if progress.cancelled() {
        return Err(cancelled_error());
    }
    let meta = fs::symlink_metadata(from)?;

    if meta.is_symlink() {
        symlink(fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir(to)?;
        for item in fs::read_dir(from)? {
            let item = item?;
            copy_path(&item.path(), &to.join(item.file_name()), progress)?;
        }
        // Last, so that read-only folders can still be filled.
        let _ = fs::set_permissions(to, meta.permissions());
        Ok(())
    } else if meta.is_file() {
        copy_file(from, to, &meta, progress)
    } else {
        Ok(()) // pipes, sockets and devices are skipped on purpose
    }
}

fn copy_file(from: &Path, to: &Path, meta: &fs::Metadata, progress: &Progress) -> io::Result<()> {
    let mut source = File::open(from)?;
    // create_new: never silently overwrite something that is already there.
    let mut target = OpenOptions::new().write(true).create_new(true).open(to)?;

    if let Err(e) = stream(&mut source, &mut target, progress) {
        drop(target);
        let _ = fs::remove_file(to);
        return Err(e);
    }

    let _ = fs::set_permissions(to, meta.permissions());
    if let Ok(modified) = meta.modified() {
        let _ = target.set_modified(modified);
    }
    Ok(())
}

fn stream(source: &mut File, target: &mut File, progress: &Progress) -> io::Result<()> {
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if progress.cancelled() {
            return Err(cancelled_error());
        }
        let read = match source.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        target.write_all(&buffer[..read])?;
        progress.done.fetch_add(read as u64, Ordering::Relaxed);
    }
}

/// Move by renaming; if that crosses drives, copy and then delete the original.
pub fn move_path(from: &Path, to: &Path, progress: &Progress) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(e) if e.raw_os_error() == Some(EXDEV) => {
            if let Err(e) = copy_path(from, to, progress) {
                if e.kind() != io::ErrorKind::AlreadyExists {
                    let _ = remove_path(to); // don't leave half a copy behind
                }
                return Err(e);
            }
            remove_path(from)
        }
        other => other,
    }
}

/// Rename or move several items at once, safely.
///
/// Works even when names swap (a→b and b→a) or chain (a→b and b→c): in that case
/// everything is first parked under temporary names. Nothing existing is overwritten.
pub fn rename_many(pairs: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    let sources: HashSet<&Path> = pairs.iter().map(|(from, _)| from.as_path()).collect();

    let mut targets: HashSet<&Path> = HashSet::new();
    for (_, to) in pairs {
        if !targets.insert(to.as_path()) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "two items would get the same name"));
        }
        if exists(to) && !sources.contains(to.as_path()) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", to.display())));
        }
    }

    let progress = Progress::default();
    let overlap = pairs.iter().any(|(_, to)| sources.contains(to.as_path()));
    if !overlap {
        for (from, to) in pairs {
            move_path(from, to, &progress)?;
        }
        return Ok(());
    }

    let tag = std::process::id();
    let mut parked = Vec::new();
    for (i, (from, to)) in pairs.iter().enumerate() {
        let dir = from.parent().unwrap_or_else(|| Path::new("."));
        let temp = dir.join(format!(".yatfm-tmp-{tag}-{i}"));
        fs::rename(from, &temp)?;
        parked.push((temp, to));
    }
    for (temp, to) in parked {
        move_path(&temp, to, &progress)?;
    }
    Ok(())
}

/// Delete for good. Never follows symlinks.
pub fn remove_path(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
