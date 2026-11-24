//! A fuzzy picker: type to narrow a list, Enter to choose.
//! Used for "jump to a frequent folder" and "find a file below here".

use std::{
    collections::VecDeque,
    env, fs,
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, TryRecvError},
    thread,
};

/// Never draw more rows than this; nobody scrolls through thousands of matches.
const MAX_SHOWN: usize = 300;
/// Stop searching after this many files, so a search from `/` can't eat all memory.
const MAX_FOUND: usize = 100_000;
const SKIP_DIRS: [&str; 3] = [".git", "node_modules", "target"];

pub struct Item {
    pub path: PathBuf,
    /// What is shown (and matched against): relative to the search root, or `~/...`.
    pub label: String,
    pub is_dir: bool,
    lower: String,
}

pub struct Picker {
    pub title: String,
    pub query: String,
    pub items: Vec<Item>,
    /// Indices into `items`, best match first.
    pub shown: Vec<usize>,
    pub selected: usize,
    /// True while the background search is still running.
    pub loading: bool,
    base: Option<PathBuf>,
    rx: Option<Receiver<Vec<(PathBuf, bool)>>>,
}

impl Picker {
    fn empty(title: &str, base: Option<PathBuf>) -> Picker {
        Picker {
            title: title.to_string(),
            query: String::new(),
            items: Vec::new(),
            shown: Vec::new(),
            selected: 0,
            loading: false,
            base,
            rx: None,
        }
    }

    /// A picker over a fixed list of folders.
    pub fn with_items(title: &str, folders: Vec<PathBuf>) -> Picker {
        let mut picker = Picker::empty(title, None);
        picker.add(folders.into_iter().map(|path| (path, true)).collect());
        picker.refilter();
        picker
    }

    /// A picker over everything below `root`, filled in by a background thread.
    pub fn find(title: &str, root: PathBuf, show_hidden: bool) -> Picker {
        let rx = walk(root.clone(), show_hidden);
        let mut picker = Picker::empty(title, Some(root));
        picker.rx = Some(rx);
        picker.loading = true;
        picker
    }

    fn add(&mut self, found: Vec<(PathBuf, bool)>) {
        for (path, is_dir) in found {
            let mut label = label_for(&path, self.base.as_deref());
            if is_dir && !label.ends_with('/') {
                label.push('/');
            }
            let lower = label.to_lowercase();
            self.items.push(Item { path, label, is_dir, lower });
        }
    }

    /// Pick up whatever the background search has found since last time.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        let mut found = Vec::new();
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(batch) => found.extend(batch),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.rx = None;
            self.loading = false;
        }
        if !found.is_empty() {
            self.add(found);
            self.refilter();
        }
    }

    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.shown = (0..self.items.len().min(MAX_SHOWN)).collect();
        } else {
            let query_text = self.query.to_lowercase();
            let query: Vec<char> = query_text.chars().collect();
            let mut scored: Vec<(i32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| fuzzy_score(&query, &query_text, &item.lower).map(|score| (score, i)))
                .collect();
            // Best score first; ties keep their original order.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            scored.truncate(MAX_SHOWN);
            self.shown = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.refilter();
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.refilter();
    }

    pub fn step(&mut self, delta: isize) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    pub fn chosen(&self) -> Option<&Item> {
        self.shown.get(self.selected).map(|&i| &self.items[i])
    }
}

fn label_for(path: &Path, base: Option<&Path>) -> String {
    if let Some(base) = base {
        if let Ok(relative) = path.strip_prefix(base) {
            return relative.display().to_string();
        }
    }
    if let Some(home) = env::var_os("HOME") {
        if let Ok(rest) = path.strip_prefix(PathBuf::from(home)) {
            return if rest.as_os_str().is_empty() { "~".to_string() } else { format!("~/{}", rest.display()) };
        }
    }
    path.display().to_string()
}

/// Does every letter of `query` appear in `text`, in order? If so, how good is the match?
/// Rewards runs of consecutive letters, matches at the start of words, and matches in the
/// file name itself. Slightly prefers shorter paths.
fn fuzzy_score(query: &[char], query_text: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let mut score = 0i32;
    let mut wanted = 0;
    let mut last_hit: Option<usize> = None;
    let mut previous = '/';

    for (i, c) in text.chars().enumerate() {
        if wanted < query.len() && c == query[wanted] {
            score += 10;
            if i > 0 && last_hit == Some(i - 1) {
                score += 15;
            }
            if matches!(previous, '/' | '_' | '-' | '.' | ' ') {
                score += 12;
            }
            wanted += 1;
            last_hit = Some(i);
        }
        previous = c;
    }
    if wanted < query.len() {
        return None;
    }

    let name = text.trim_end_matches('/').rsplit('/').next().unwrap_or(text);
    if name.contains(query_text) {
        score += 40;
        if name.starts_with(query_text) {
            score += 20;
        }
    }
    score -= (text.chars().count() / 4) as i32;
    Some(score)
}

/// Breadth-first, so shallow files show up first. Runs on its own thread.
/// Symlinked folders are listed but not entered, which avoids loops.
fn walk(root: PathBuf, show_hidden: bool) -> Receiver<Vec<(PathBuf, bool)>> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let mut queue = VecDeque::from([root]);
        let mut batch: Vec<(PathBuf, bool)> = Vec::new();
        let mut found = 0usize;

        'walk: while let Some(dir) = queue.pop_front() {
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            for item in read.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                if !show_hidden && name.starts_with('.') {
                    continue;
                }
                let Ok(kind) = item.file_type() else {
                    continue;
                };
                let path = item.path();
                if kind.is_dir() {
                    if SKIP_DIRS.contains(&name.as_str()) {
                        continue;
                    }
                    queue.push_back(path.clone());
                }
                batch.push((path, kind.is_dir()));
                found += 1;

                if batch.len() >= 500 && tx.send(std::mem::take(&mut batch)).is_err() {
                    return; // the picker was closed
                }
                if found >= MAX_FOUND {
                    break 'walk;
                }
            }
        }
        let _ = tx.send(batch);
    });
    rx
}
