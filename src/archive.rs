//! Extracting and compressing archives by calling the standard tools
//! (tar, unzip, zip, bsdtar, 7z, unrar), so we need no extra libraries.

use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::fsops;

const TAR_SUFFIXES: [&str; 10] =
    [".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tbz", ".tar.xz", ".txz", ".tar.zst", ".tzst"];
const OTHER_SUFFIXES: [&str; 3] = [".zip", ".7z", ".rar"];

enum Kind {
    Tar,
    Zip,
    SevenZip,
    Rar,
}

fn kind_of(name: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    if TAR_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
        Some(Kind::Tar)
    } else if lower.ends_with(".zip") {
        Some(Kind::Zip)
    } else if lower.ends_with(".7z") {
        Some(Kind::SevenZip)
    } else if lower.ends_with(".rar") {
        Some(Kind::Rar)
    } else {
        None
    }
}

pub fn is_archive(name: &str) -> bool {
    kind_of(name).is_some()
}

/// "photos.tar.gz" -> "photos"
fn folder_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let suffix = TAR_SUFFIXES.iter().chain(OTHER_SUFFIXES.iter()).find(|s| lower.ends_with(**s));
    match suffix {
        Some(s) if s.len() < name.len() => name[..name.len() - s.len()].to_string(),
        _ => format!("{name}.extracted"),
    }
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Try each command in turn; skip tools that aren't installed.
fn run_first(candidates: &[Vec<String>], dir: &Path) -> Result<(), String> {
    let mut last_error = String::from("no tool found for that (install tar, unzip, zip or 7z)");
    for command in candidates {
        let Some((program, args)) = command.split_first() else {
            continue;
        };
        match Command::new(program).args(args).current_dir(dir).stdin(Stdio::null()).output() {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => {
                let problem = String::from_utf8_lossy(&out.stderr);
                last_error = format!("{program}: {}", problem.lines().next().unwrap_or("failed"));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => last_error = format!("{program}: {e}"),
        }
    }
    Err(last_error)
}

/// Extract `archive` into a new folder inside `parent`, named after the archive.
/// Returns the new folder.
pub fn extract(archive: &Path, parent: &Path) -> Result<PathBuf, String> {
    let file = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(kind) = kind_of(&file) else {
        return Err(format!("{file}: not a supported archive (zip, tar.*, 7z, rar)"));
    };

    let target = fsops::unique_path(parent, OsStr::new(&folder_name(&file)));
    fs::create_dir(&target).map_err(|e| e.to_string())?;

    let a = archive.display().to_string();
    let t = target.display().to_string();
    let seven = |program: &str| words(&[program, "x", "-y", &format!("-o{t}"), &a]);
    let bsdtar = words(&["bsdtar", "-xf", &a, "-C", &t]);

    let candidates: Vec<Vec<String>> = match kind {
        Kind::Tar => vec![words(&["tar", "-xf", &a, "-C", &t]), bsdtar],
        Kind::Zip => vec![words(&["unzip", "-q", &a, "-d", &t]), bsdtar, seven("7z")],
        Kind::SevenZip => vec![seven("7z"), seven("7zz"), bsdtar],
        Kind::Rar => vec![words(&["unrar", "x", "-y", &a, &format!("{t}/")]), bsdtar, seven("7z")],
    };

    match run_first(&candidates, parent) {
        Ok(()) => Ok(target),
        Err(e) => {
            let _ = fsops::remove_path(&target);
            Err(format!("{file}: {e}"))
        }
    }
}

/// Compress `items` (all inside `dir`) into `dir/archive_name`.
/// The format comes from the name: .zip, .tar.gz, .tar.xz, .tar.zst, .tar.bz2, .tar or .7z
pub fn compress(items: &[PathBuf], dir: &Path, archive_name: &str) -> Result<PathBuf, String> {
    let out = dir.join(archive_name);
    if fsops::exists(&out) {
        return Err(format!("{archive_name} already exists"));
    }

    // "./" keeps names that start with "-" from being read as options.
    let names: Vec<String> = items
        .iter()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .map(|n| if n.starts_with('-') { format!("./{n}") } else { n })
        .collect();
    if names.is_empty() {
        return Err("nothing to compress".into());
    }

    let with_names = |head: &[&str]| {
        let mut command = words(head);
        command.extend(names.iter().cloned());
        command
    };

    let lower = archive_name.to_lowercase();
    let candidates = if lower.ends_with(".zip") {
        vec![
            with_names(&["zip", "-r", "-q", archive_name]),
            with_names(&["bsdtar", "-acf", archive_name]),
            with_names(&["7z", "a", "-y", archive_name]),
        ]
    } else if TAR_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
        vec![with_names(&["tar", "-acf", archive_name]), with_names(&["bsdtar", "-acf", archive_name])]
    } else if lower.ends_with(".7z") {
        vec![with_names(&["7z", "a", "-y", archive_name])]
    } else {
        return Err("Name it .zip, .tar.gz, .tar.xz, .tar.zst, .tar.bz2 or .7z".into());
    };

    match run_first(&candidates, dir) {
        Ok(()) => Ok(out),
        Err(e) => {
            let _ = fsops::remove_path(&out);
            Err(format!("{archive_name}: {e}"))
        }
    }
}
