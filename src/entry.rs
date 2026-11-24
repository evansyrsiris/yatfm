//! One item in a folder, and how to read a folder from disk.

use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub mode: u32,
}

impl Entry {
    pub fn is_executable(&self) -> bool {
        self.is_file && self.mode & 0o111 != 0
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum SortBy {
    Name,
    Size,
    Modified,
}

impl SortBy {
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Size => "size",
            SortBy::Modified => "modified",
        }
    }
}

/// Read a folder. Folders always come first; unreadable items are skipped.
pub fn read_dir(dir: &Path, show_hidden: bool, sort: SortBy, reverse: bool) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();

    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }

        let path = item.path();
        let Ok(link_meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let is_symlink = link_meta.is_symlink();
        // Follow symlinks, so a link to a folder behaves like a folder.
        let meta = fs::metadata(&path).unwrap_or(link_meta);

        entries.push(Entry {
            name,
            path,
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
            is_symlink,
            size: meta.len(),
            modified: meta.modified().ok(),
            mode: meta.permissions().mode(),
        });
    }

    entries.sort_by(|a, b| {
        let order = match sort {
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Size => b.size.cmp(&a.size),             // biggest first
            SortBy::Modified => b.modified.cmp(&a.modified), // newest first
        };
        let order = if reverse { order.reverse() } else { order };
        b.is_dir.cmp(&a.is_dir).then(order)
    });

    Ok(entries)
}
