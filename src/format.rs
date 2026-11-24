//! Turning numbers into text people can read.

use std::time::SystemTime;

use crate::entry::Entry;

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// "3 days ago", "just now", ...
pub fn ago(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "unknown time".into();
    };
    let Ok(age) = SystemTime::now().duration_since(time) else {
        return "in the future".into();
    };

    let secs = age.as_secs();
    let (n, unit) = match secs {
        0..=59 => return "just now".into(),
        60..=3_599 => (secs / 60, "minute"),
        3_600..=86_399 => (secs / 3_600, "hour"),
        86_400..=2_591_999 => (secs / 86_400, "day"),
        2_592_000..=31_535_999 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    format!("{} ago", plural(n as usize, unit))
}

/// "1 item", "3 items".
pub fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// "drwxr-xr-x"
pub fn mode_string(entry: &Entry) -> String {
    let kind = if entry.is_symlink {
        'l'
    } else if entry.is_dir {
        'd'
    } else {
        '-'
    };
    let mut text = String::from(kind);
    for shift in [6, 3, 0] {
        let bits = (entry.mode >> shift) & 0o7;
        text.push(if bits & 4 != 0 { 'r' } else { '-' });
        text.push(if bits & 2 != 0 { 'w' } else { '-' });
        text.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    text
}
