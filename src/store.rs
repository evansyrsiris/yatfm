//! Things yatfm remembers between runs (bookmarks, visited folders) and the optional config file.
//!
//! Plain text files, so no extra dependencies:
//!   ~/.local/share/yatfm/bookmarks   one `key<TAB>path` per line
//!   ~/.local/share/yatfm/visits      one `count<TAB>last-visit<TAB>path` per line
//!   ~/.config/yatfm/config           see `Config::load`

use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::entry::SortBy;

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(variable: &str, fallback: &str) -> PathBuf {
    match env::var_os(variable) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("yatfm"),
        _ => home().join(fallback).join("yatfm"),
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Paths containing these would break the line-based files above, so we don't store them.
fn storable(path: &Path) -> bool {
    path.to_str().map_or(false, |text| !text.contains(|c: char| c == '\t' || c == '\n'))
}

struct Visit {
    count: u32,
    last: u64,
}

/// "Frecency": how often *and* how recently. Recent visits count for more.
fn score(visit: &Visit) -> f64 {
    let age = now().saturating_sub(visit.last);
    let weight = match age {
        0..=3_599 => 4.0,
        3_600..=86_399 => 2.0,
        86_400..=604_799 => 0.5,
        _ => 0.25,
    };
    visit.count as f64 * weight
}

pub struct Store {
    pub bookmarks: BTreeMap<char, PathBuf>,
    visits: HashMap<PathBuf, Visit>,
}

impl Store {
    pub fn load() -> Store {
        let mut store = Store { bookmarks: BTreeMap::new(), visits: HashMap::new() };

        if let Ok(text) = fs::read_to_string(data_dir().join("bookmarks")) {
            for line in text.lines() {
                if let Some((key, path)) = line.split_once('\t') {
                    if let Some(c) = key.chars().next() {
                        store.bookmarks.insert(c, PathBuf::from(path));
                    }
                }
            }
        }

        if let Ok(text) = fs::read_to_string(data_dir().join("visits")) {
            for line in text.lines() {
                let mut parts = line.splitn(3, '\t');
                let (Some(count), Some(last), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
                    continue;
                };
                let (Ok(count), Ok(last)) = (count.parse::<u32>(), last.parse::<u64>()) else {
                    continue;
                };
                store.visits.insert(PathBuf::from(path), Visit { count, last });
            }
        }
        store
    }

    pub fn save(&self) {
        let dir = data_dir();
        if fs::create_dir_all(&dir).is_err() {
            return;
        }

        let bookmarks: String = self.bookmarks.iter().map(|(key, path)| format!("{key}\t{}\n", path.display())).collect();
        let _ = fs::write(dir.join("bookmarks"), bookmarks);

        // Keep the 1000 best folders so the file never grows forever.
        let mut ranked: Vec<(&PathBuf, &Visit)> = self.visits.iter().collect();
        ranked.sort_by(|a, b| score(b.1).total_cmp(&score(a.1)));
        ranked.truncate(1000);
        let visits: String =
            ranked.iter().map(|(path, v)| format!("{}\t{}\t{}\n", v.count, v.last, path.display())).collect();
        let _ = fs::write(dir.join("visits"), visits);
    }

    /// Returns false if the path can't be stored.
    pub fn set_bookmark(&mut self, key: char, path: &Path) -> bool {
        if key == '\t' || key == '\n' || !storable(path) {
            return false;
        }
        self.bookmarks.insert(key, path.to_path_buf());
        true
    }

    pub fn visit(&mut self, path: &Path) {
        if !storable(path) {
            return;
        }
        let entry = self.visits.entry(path.to_path_buf()).or_insert(Visit { count: 0, last: 0 });
        entry.count += 1;
        entry.last = now();
    }

    /// Folders that still exist, best first.
    pub fn ranked(&self) -> Vec<PathBuf> {
        let mut list: Vec<(&PathBuf, f64)> = self.visits.iter().map(|(path, v)| (path, score(v))).collect();
        list.sort_by(|a, b| b.1.total_cmp(&a.1));
        list.into_iter().map(|(path, _)| path.clone()).filter(|path| path.is_dir()).collect()
    }
}

// ---------------------------------------------------------------------- config

/// "Open files with this program" rule.
pub struct Opener {
    pub extensions: Vec<String>,
    /// True: runs inside the terminal (like vim). False: starts a window and lets go.
    pub terminal: bool,
    pub command: Vec<String>,
}

pub struct Config {
    pub show_hidden: bool,
    pub sort: SortBy,
    pub reverse: bool,
    pub openers: Vec<Opener>,
}

impl Config {
    /// Reads `~/.config/yatfm/config`. Every line is optional:
    ///
    /// ```text
    /// # comments start with #
    /// show_hidden = true
    /// sort = modified              # name, size or modified
    /// reverse = false
    /// open md,txt,rs = term nvim   # runs inside the terminal
    /// open png,jpg,gif = gui imv   # starts its own window
    /// ```
    pub fn load() -> Config {
        let mut config = Config { show_hidden: false, sort: SortBy::Name, reverse: false, openers: Vec::new() };
        let Ok(text) = fs::read_to_string(config_dir().join("config")) else {
            return config;
        };

        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((left, right)) = line.split_once('=') else {
                continue;
            };
            let (left, right) = (left.trim(), right.trim());

            match left {
                "show_hidden" => config.show_hidden = right == "true",
                "reverse" => config.reverse = right == "true",
                "sort" => {
                    config.sort = match right {
                        "size" => SortBy::Size,
                        "modified" => SortBy::Modified,
                        _ => SortBy::Name,
                    }
                }
                _ => {
                    let Some(extensions) = left.strip_prefix("open ") else {
                        continue;
                    };
                    let mut words = right.split_whitespace();
                    let terminal = match words.next() {
                        Some("term") => true,
                        Some("gui") => false,
                        _ => continue,
                    };
                    let command: Vec<String> = words.map(String::from).collect();
                    if command.is_empty() {
                        continue;
                    }
                    let extensions =
                        extensions.split(',').map(|e| e.trim().trim_start_matches('.').to_lowercase()).collect();
                    config.openers.push(Opener { extensions, terminal, command });
                }
            }
        }
        config
    }

    pub fn opener_for(&self, extension: &str) -> Option<&Opener> {
        self.openers.iter().find(|opener| opener.extensions.iter().any(|e| e == extension))
    }
}
