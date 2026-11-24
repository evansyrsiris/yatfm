use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    widgets::ListState,
};

use crate::{
    archive,
    entry::{self, Entry, SortBy},
    format::plural,
    fsops,
    jobs::{Job, Jobs, OnConflict},
    keymap::{self, Action, Lookup},
    picker::Picker,
    preview::{Preview, PreviewLoader},
    store::{home, Config, Store},
    system,
    undo::Undo,
};

pub enum Mode {
    Normal,
    Visual {
        anchor: usize,
    },
    Input(Prompt),
    Confirm(Confirm),
    Conflict(Conflict),
    Picker(Picker),
    Bookmark(BookmarkMode),
    Help,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BookmarkMode {
    Set,
    Go,
}

pub enum PromptKind {
    Rename(PathBuf),
    NewFile,
    NewFolder,
    Filter,
    Command,
    Shell,
    OpenWith(PathBuf),
    Compress,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
    pub cursor: usize,
    pub hint: String,
}

impl Prompt {
    fn new(kind: PromptKind, text: &str) -> Prompt {
        Prompt { kind, text: text.to_string(), cursor: text.chars().count(), hint: String::new() }
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Position in the string of the `index`-th character.
    fn byte(&self, index: usize) -> usize {
        self.text.char_indices().nth(index).map(|(byte, _)| byte).unwrap_or(self.text.len())
    }

    fn insert(&mut self, c: char) {
        let at = self.byte(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            let at = self.byte(self.cursor - 1);
            self.text.remove(at);
            self.cursor -= 1;
        }
    }

    fn delete(&mut self) {
        let at = self.byte(self.cursor);
        if at < self.text.len() {
            self.text.remove(at);
        }
    }

    fn clear_before_cursor(&mut self) {
        let at = self.byte(self.cursor);
        self.text.drain(..at);
        self.cursor = 0;
    }

    fn delete_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor.min(chars.len());
        while start > 0 && chars[start - 1] == ' ' {
            start -= 1;
        }
        while start > 0 && chars[start - 1] != ' ' && chars[start - 1] != '/' {
            start -= 1;
        }
        let mut text: String = chars[..start].iter().collect();
        text.extend(chars[self.cursor.min(chars.len())..].iter());
        self.text = text;
        self.cursor = start;
    }
}

pub struct Confirm {
    pub question: String,
    /// Started if you answer "y".
    pub job: Job,
}

pub struct Conflict {
    pub clip: Clipboard,
    pub dest: PathBuf,
    /// How many of the pasted items already exist here.
    pub clashes: usize,
}

#[derive(Clone)]
pub struct Clipboard {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

/// A request to give the terminal to another program (editor, shell...) for a while.
/// `main` does the actual hand-over.
pub struct Handoff {
    pub command: Vec<String>,
    /// Show "press Enter to return" afterwards, so you can read the output.
    pub wait: bool,
}

/// The temp file we gave the editor for a bulk rename, and what it names.
struct BulkRename {
    file: PathBuf,
    items: Vec<PathBuf>,
}

#[derive(Clone, Copy)]
enum History {
    Push,
    Back,
    Forward,
}

pub struct App {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    /// Cursor position and scroll offset of the middle column.
    pub list: ListState,
    pub parent_entries: Vec<Entry>,
    pub parent_selected: Option<usize>,
    pub preview: Preview,

    pub marked: HashSet<PathBuf>,
    pub clipboard: Option<Clipboard>,
    pub undo_stack: Vec<Undo>,

    pub mode: Mode,
    /// Keys typed so far in a multi-key binding, e.g. "g".
    pub typed: String,
    /// A number typed before a command, e.g. the 5 in "5j".
    pub count: usize,
    pub status: String,
    pub help_scroll: u16,
    /// Rows visible in the middle column. Set by the UI on every frame.
    pub page: usize,

    pub show_hidden: bool,
    pub sort: SortBy,
    pub reverse: bool,
    pub filter: String,

    pub jobs: Jobs,
    pub store: Store,
    pub config: Config,
    pub should_quit: bool,
    /// Set when we want the terminal handed to another program. `main` does it.
    pub handoff: Option<Handoff>,

    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Which item the cursor was on, per folder, so coming back feels natural.
    remembered: HashMap<PathBuf, String>,
    bulk: Option<BulkRename>,

    preview_loader: PreviewLoader,
    preview_id: u64,
    preview_pending: bool,
    preview_requested: Instant,

    cwd_mtime: Option<SystemTime>,
    last_check: Instant,
}

impl App {
    pub fn new(start: PathBuf) -> Self {
        let config = Config::load();
        let (show_hidden, sort, reverse) = (config.show_hidden, config.sort, config.reverse);

        let mut app = App {
            cwd: start,
            entries: Vec::new(),
            list: ListState::default(),
            parent_entries: Vec::new(),
            parent_selected: None,
            preview: Preview::Empty,
            marked: HashSet::new(),
            clipboard: None,
            undo_stack: Vec::new(),
            mode: Mode::Normal,
            typed: String::new(),
            count: 0,
            status: String::new(),
            help_scroll: 0,
            page: 20,
            show_hidden,
            sort,
            reverse,
            filter: String::new(),
            jobs: Jobs::new(),
            store: Store::load(),
            config,
            should_quit: false,
            handoff: None,
            back: Vec::new(),
            forward: Vec::new(),
            remembered: HashMap::new(),
            bulk: None,
            preview_loader: PreviewLoader::new(),
            preview_id: 0,
            preview_pending: false,
            preview_requested: Instant::now(),
            cwd_mtime: None,
            last_check: Instant::now(),
        };
        app.load_entries();
        app.refresh_parent();
        app.set_cursor(0);
        app.store.visit(&app.cwd);
        app
    }

    /// Called when quitting.
    pub fn save_state(&self) {
        self.store.save();
    }

    /// Called many times a second: pick up news from everything running in the background.
    pub fn tick(&mut self) {
        self.poll_jobs();
        self.poll_preview();
        if let Mode::Picker(picker) = &mut self.mode {
            picker.poll();
        }
        self.check_disk();
    }

    // ----------------------------------------------------------------- reading

    pub fn current(&self) -> Option<&Entry> {
        self.list.selected().and_then(|i| self.entries.get(i))
    }

    pub fn cursor(&self) -> usize {
        self.list.selected().unwrap_or(0)
    }

    /// True if the preview pane should be blanked because the real one is taking a while.
    /// (For the first moments we keep showing the old one, so scrolling doesn't flicker.)
    pub fn preview_loading(&self) -> bool {
        self.preview_pending && self.preview_requested.elapsed() > Duration::from_millis(150)
    }

    /// Re-read the folder from disk, keeping the cursor where it was.
    pub fn reload(&mut self) {
        let old_index = self.cursor();
        let old_name = self.current().map(|e| e.name.clone());

        self.load_entries();
        self.refresh_parent();

        match old_name.and_then(|name| self.entries.iter().position(|e| e.name == name)) {
            Some(i) => self.set_cursor(i),
            None => self.set_cursor(old_index), // it's gone: stay near the same spot
        }
    }

    fn load_entries(&mut self) {
        self.cwd_mtime = fs::metadata(&self.cwd).and_then(|m| m.modified()).ok();
        match entry::read_dir(&self.cwd, self.show_hidden, self.sort, self.reverse) {
            Ok(mut list) => {
                if !self.filter.is_empty() {
                    let wanted = self.filter.to_lowercase();
                    list.retain(|e| e.name.to_lowercase().contains(&wanted));
                }
                self.entries = list;
            }
            Err(e) => {
                self.entries.clear();
                self.status = format!("Can't read {}: {e}", self.cwd.display());
            }
        }
    }

    fn refresh_parent(&mut self) {
        self.parent_entries = match self.cwd.parent() {
            Some(parent) => entry::read_dir(parent, self.show_hidden, self.sort, self.reverse).unwrap_or_default(),
            None => Vec::new(),
        };
        self.parent_selected = self.parent_entries.iter().position(|e| e.path == self.cwd);
    }

    /// Once a second: did something outside yatfm change this folder? If so, reload.
    fn check_disk(&mut self) {
        if self.last_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_check = Instant::now();
        let modified = fs::metadata(&self.cwd).and_then(|m| m.modified()).ok();
        if modified != self.cwd_mtime {
            self.reload();
        }
    }

    fn poll_preview(&mut self) {
        if let Some((id, preview)) = self.preview_loader.poll() {
            if id == self.preview_id {
                self.preview = preview;
                self.preview_pending = false;
            }
        }
    }

    // ------------------------------------------------------------------ cursor

    fn set_cursor(&mut self, index: usize) {
        if self.entries.is_empty() {
            self.list.select(None);
        } else {
            self.list.select(Some(index.min(self.entries.len() - 1)));
        }

        // In visual mode, the selection follows the cursor.
        if let Mode::Visual { anchor } = self.mode {
            if !self.entries.is_empty() {
                let (low, high) = (anchor.min(self.cursor()), anchor.max(self.cursor()));
                let high = high.min(self.entries.len() - 1);
                self.marked = self.entries[low..=high].iter().map(|e| e.path.clone()).collect();
            }
        }

        self.request_preview();
    }

    fn request_preview(&mut self) {
        self.preview_id += 1;
        match self.current().cloned() {
            Some(entry) => {
                self.preview_loader.request(self.preview_id, entry, self.show_hidden);
                self.preview_pending = true;
                self.preview_requested = Instant::now();
            }
            None => {
                self.preview = Preview::Empty;
                self.preview_pending = false;
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        let target = (self.cursor() as isize + delta).clamp(0, last);
        self.set_cursor(target as usize);
    }

    fn select_name(&mut self, name: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.set_cursor(i);
        }
    }

    // -------------------------------------------------------------- navigation

    /// Go into `dir`. If `select` is given, put the cursor on that name.
    fn go_to(&mut self, dir: PathBuf, select: Option<String>) {
        self.navigate(dir, select, History::Push);
    }

    fn navigate(&mut self, dir: PathBuf, select: Option<String>, how: History) {
        if let Err(e) = fs::read_dir(&dir) {
            self.status = format!("Can't open {}: {e}", dir.display());
            return;
        }

        if dir != self.cwd {
            // Remember where the cursor was, for when we come back.
            let name = self.current().map(|e| e.name.clone());
            if let Some(name) = name {
                self.remembered.insert(self.cwd.clone(), name);
            }
            match how {
                History::Push => {
                    self.back.push(self.cwd.clone());
                    self.forward.clear();
                }
                History::Back => self.forward.push(self.cwd.clone()),
                History::Forward => self.back.push(self.cwd.clone()),
            }
            if self.back.len() > 200 {
                self.back.remove(0);
            }
            self.store.visit(&dir);
        }

        let select = select.or_else(|| self.remembered.get(&dir).cloned());
        self.cwd = dir;
        self.filter.clear();
        self.marked.clear();
        self.mode = Mode::Normal;
        self.load_entries();
        self.refresh_parent();
        self.set_cursor(0);
        if let Some(name) = select {
            self.select_name(&name);
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            let came_from = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            self.go_to(parent.to_path_buf(), came_from);
        }
    }

    fn go_back(&mut self) {
        match self.back.pop() {
            Some(dir) => self.navigate(dir, None, History::Back),
            None => self.status = "Nothing to go back to".into(),
        }
    }

    fn go_forward(&mut self) {
        match self.forward.pop() {
            Some(dir) => self.navigate(dir, None, History::Forward),
            None => self.status = "Nothing to go forward to".into(),
        }
    }

    /// Show a picked path: enter it if it's a folder, otherwise go to its folder and select it.
    fn reveal(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.go_to(path, None);
        } else if let Some(parent) = path.parent() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            self.go_to(parent.to_path_buf(), name);
        }
    }

    fn open_current(&mut self) {
        let Some(entry) = self.current() else { return };
        let (path, is_dir, is_file) = (entry.path.clone(), entry.is_dir, entry.is_file);

        if is_dir {
            self.go_to(path, None);
        } else if is_file {
            self.open_file(path, false);
        } else {
            self.status = "Can't open a special file".into();
        }
    }

    /// Your `open` rules from the config first, then the editor for text files,
    /// then whatever the desktop wants. `system_default` skips straight to the last.
    fn open_file(&mut self, path: PathBuf, system_default: bool) {
        if !system_default {
            let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let rule = self.config.opener_for(&extension).map(|o| (o.terminal, o.command.clone()));

            if let Some((terminal, mut command)) = rule {
                command.push(path.to_string_lossy().into_owned());
                if terminal {
                    self.handoff = Some(Handoff { command, wait: false });
                } else if let Err(problem) = system::spawn_detached(&command) {
                    self.status = problem;
                }
                return;
            }

            if system::looks_like_text(&path) {
                let mut command = system::editor_words();
                command.push(path.to_string_lossy().into_owned());
                self.handoff = Some(Handoff { command, wait: false });
                return;
            }
        }

        if let Err(e) = open::that_detached(&path) {
            self.status = format!("Couldn't open it: {e}");
        }
    }

    // --------------------------------------------------------------- selection

    /// What an action applies to: the selected items, or else the one under the cursor.
    fn targets(&self) -> Vec<PathBuf> {
        let selected: Vec<PathBuf> =
            self.entries.iter().filter(|e| self.marked.contains(&e.path)).map(|e| e.path.clone()).collect();
        if !selected.is_empty() {
            return selected;
        }
        self.current().map(|e| vec![e.path.clone()]).unwrap_or_default()
    }

    // -------------------------------------------------------------------- keys

    pub fn on_key(&mut self, key: KeyEvent) {
        self.status.clear();

        // Take the mode out, so each handler can decide what the next mode is.
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_key(key),
            Mode::Visual { anchor } => {
                self.mode = Mode::Visual { anchor };
                self.normal_key(key);
            }
            Mode::Input(prompt) => self.input_key(prompt, key),
            Mode::Confirm(confirm) => self.confirm_key(confirm, key),
            Mode::Conflict(conflict) => self.conflict_key(conflict, key),
            Mode::Picker(picker) => self.picker_key(picker, key),
            Mode::Bookmark(kind) => self.bookmark_key(kind, key),
            Mode::Help => self.help_key(key),
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        // A number before a command repeats it: 5j moves five down, 12G goes to line 12.
        if self.typed.is_empty() && key.modifiers.is_empty() {
            if let KeyCode::Char(c @ '0'..='9') = key.code {
                if c != '0' || self.count > 0 {
                    let digit = c.to_digit(10).unwrap_or(0) as usize;
                    self.count = (self.count * 10 + digit).min(9999);
                    return;
                }
            }
        }

        let Some(name) = keymap::key_name(key) else {
            return;
        };
        self.typed.push_str(&name);

        match keymap::lookup(&self.typed) {
            Lookup::Run(action) => {
                self.typed.clear();
                let given = self.count > 0;
                let count = std::mem::take(&mut self.count).max(1);
                match action {
                    Action::Bottom if given => self.set_cursor(count - 1),
                    Action::Down | Action::Up | Action::Mark | Action::HalfPageDown | Action::HalfPageUp => {
                        for _ in 0..count {
                            self.run(action);
                        }
                    }
                    _ => self.run(action),
                }
            }
            Lookup::Pending => {} // the UI shows the which-key popup
            Lookup::Unknown => {
                self.typed.clear();
                self.count = 0;
            }
        }
    }

    fn input_key(&mut self, mut prompt: Prompt, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        prompt.hint.clear();

        match key.code {
            KeyCode::Esc => return self.cancel_prompt(prompt),
            KeyCode::Char('c') if ctrl => return self.cancel_prompt(prompt),
            KeyCode::Enter => return self.submit(prompt),
            KeyCode::Backspace => {
                if prompt.text.is_empty() {
                    return self.cancel_prompt(prompt); // backspace on an empty line leaves it
                }
                prompt.backspace();
            }
            KeyCode::Delete => prompt.delete(),
            KeyCode::Left => prompt.cursor = prompt.cursor.saturating_sub(1),
            KeyCode::Right => prompt.cursor = (prompt.cursor + 1).min(prompt.len()),
            KeyCode::Home => prompt.cursor = 0,
            KeyCode::End => prompt.cursor = prompt.len(),
            KeyCode::Char('a') if ctrl => prompt.cursor = 0,
            KeyCode::Char('e') if ctrl => prompt.cursor = prompt.len(),
            KeyCode::Char('u') if ctrl => prompt.clear_before_cursor(),
            KeyCode::Char('w') if ctrl => prompt.delete_word(),
            KeyCode::Tab => self.complete(&mut prompt),
            KeyCode::Char(c) if !ctrl => prompt.insert(c),
            _ => {}
        }

        // The filter applies as you type.
        if matches!(prompt.kind, PromptKind::Filter) {
            self.set_filter(prompt.text.clone());
        }
        self.mode = Mode::Input(prompt);
    }

    /// Tab in `:cd some/pa` completes folder names.
    fn complete(&mut self, prompt: &mut Prompt) {
        if !matches!(prompt.kind, PromptKind::Command) {
            return;
        }
        let Some(arg) = prompt.text.strip_prefix("cd ").map(str::to_string) else {
            return;
        };

        let (typed_dir, partial) = match arg.rfind('/') {
            Some(i) => (&arg[..=i], &arg[i + 1..]),
            None => ("", arg.as_str()),
        };
        let real_dir = self.cwd.join(expand_tilde(typed_dir));

        let Ok(read) = fs::read_dir(&real_dir) else {
            return;
        };
        let mut names: Vec<String> = read
            .flatten()
            .filter(|item| item.path().is_dir())
            .map(|item| item.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(partial) && (partial.starts_with('.') || !name.starts_with('.')))
            .collect();
        names.sort();

        if names.is_empty() {
            prompt.hint = "no folder matches".into();
            return;
        }
        let mut best = names[0].clone();
        for name in &names[1..] {
            best = common_prefix(&best, name);
        }
        let slash = if names.len() == 1 { "/" } else { "" };
        let completed = format!("cd {typed_dir}{best}{slash}");

        if names.len() > 1 {
            prompt.hint = names.join("   ");
        }
        prompt.text = completed;
        prompt.cursor = prompt.len();
    }

    fn cancel_prompt(&mut self, prompt: Prompt) {
        if matches!(prompt.kind, PromptKind::Filter) {
            self.set_filter(String::new());
        }
    }

    fn confirm_key(&mut self, confirm: Confirm, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.marked.clear();
                self.start_job(confirm.job);
            }
            _ => self.status = "Cancelled".into(),
        }
    }

    fn conflict_key(&mut self, conflict: Conflict, key: KeyEvent) {
        let policy = match key.code {
            KeyCode::Char('o') | KeyCode::Char('O') => OnConflict::Overwrite,
            KeyCode::Char('s') | KeyCode::Char('S') => OnConflict::Skip,
            KeyCode::Char('k') | KeyCode::Char('K') | KeyCode::Enter => OnConflict::Keep,
            _ => {
                self.status = "Cancelled".into();
                return;
            }
        };
        self.begin_transfer(conflict.clip, conflict.dest, policy);
    }

    fn picker_key(&mut self, mut picker: Picker, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => return,
            KeyCode::Char('c') | KeyCode::Char('g') if ctrl => return,
            KeyCode::Enter => {
                let chosen = picker.chosen().map(|item| item.path.clone());
                if let Some(path) = chosen {
                    self.reveal(path);
                }
                return;
            }
            KeyCode::Up => picker.step(-1),
            KeyCode::Down => picker.step(1),
            KeyCode::PageUp => picker.step(-10),
            KeyCode::PageDown => picker.step(10),
            KeyCode::Char('k') | KeyCode::Char('p') if ctrl => picker.step(-1),
            KeyCode::Char('j') | KeyCode::Char('n') if ctrl => picker.step(1),
            KeyCode::Backspace => picker.backspace(),
            KeyCode::Char('u') if ctrl => picker.clear(),
            KeyCode::Char(c) if !ctrl => picker.push(c),
            _ => {}
        }
        self.mode = Mode::Picker(picker);
    }

    fn bookmark_key(&mut self, kind: BookmarkMode, key: KeyEvent) {
        let KeyCode::Char(c) = key.code else {
            return; // Esc or anything else cancels
        };
        match kind {
            BookmarkMode::Set => {
                if self.store.set_bookmark(c, &self.cwd) {
                    self.store.save();
                    self.status = format!("Bookmarked this folder as '{c}'");
                } else {
                    self.status = "Can't bookmark that".into();
                }
            }
            BookmarkMode::Go => match self.store.bookmarks.get(&c).cloned() {
                Some(path) => self.go_to(path, None),
                None => self.status = format!("No bookmark '{c}'"),
            },
        }
    }

    fn help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.help_scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
            _ => return, // any other key closes the help
        }
        self.mode = Mode::Help;
    }

    fn submit(&mut self, prompt: Prompt) {
        let text = prompt.text.trim().to_string();
        match prompt.kind {
            PromptKind::Filter => {} // already applied
            PromptKind::Command => self.run_command(&text),
            PromptKind::Rename(path) => self.rename(&path, &text),
            PromptKind::NewFile => self.create(&text, false),
            PromptKind::NewFolder => self.create(&text, true),
            PromptKind::Shell => self.run_shell(&text),
            PromptKind::OpenWith(path) => self.open_with(&path, &text),
            PromptKind::Compress => self.compress(&text),
        }
    }

    // ----------------------------------------------------------------- actions

    fn run(&mut self, action: Action) {
        let half_page = (self.page / 2).max(1) as isize;

        match action {
            Action::Down => self.move_cursor(1),
            Action::Up => self.move_cursor(-1),
            Action::HalfPageDown => self.move_cursor(half_page),
            Action::HalfPageUp => self.move_cursor(-half_page),
            Action::Top => self.set_cursor(0),
            Action::Bottom => self.set_cursor(usize::MAX),
            Action::Left => self.go_up(),
            Action::Right => self.open_current(),
            Action::Back => self.go_back(),
            Action::Forward => self.go_forward(),

            Action::GoHome => self.go_to(home(), None),
            Action::GoRoot => self.go_to(PathBuf::from("/"), None),
            Action::GoDownloads => self.go_to(home().join("Downloads"), None),
            Action::GoConfig => self.go_to(home().join(".config"), None),
            Action::Jump => self.start_jump(),
            Action::Find => {
                self.mode = Mode::Picker(Picker::find("Find a file below here", self.cwd.clone(), self.show_hidden));
            }
            Action::SetBookmark => self.mode = Mode::Bookmark(BookmarkMode::Set),
            Action::GoBookmark => self.mode = Mode::Bookmark(BookmarkMode::Go),

            Action::Mark => {
                if let Some(path) = self.current().map(|e| e.path.clone()) {
                    if !self.marked.remove(&path) {
                        self.marked.insert(path);
                    }
                    self.move_cursor(1);
                }
            }
            Action::Visual => self.toggle_visual(),
            Action::SelectAll => self.marked = self.entries.iter().map(|e| e.path.clone()).collect(),
            Action::ClearAll => self.clear_all(),

            Action::Yank => self.fill_clipboard(false),
            Action::Cut => self.fill_clipboard(true),
            Action::Paste => self.paste(),
            Action::Trash => self.ask_to_remove(false),
            Action::Delete => self.ask_to_remove(true),
            Action::Rename => self.start_rename(),
            Action::BulkRename => self.start_bulk_rename(),
            Action::NewFile => self.open_prompt(PromptKind::NewFile, ""),
            Action::NewFolder => self.open_prompt(PromptKind::NewFolder, ""),
            Action::Undo => self.undo(),
            Action::Compress => self.start_compress(),
            Action::Extract => self.extract(),

            Action::Edit => self.start_edit(),
            Action::Open => {
                if let Some(path) = self.current().map(|e| e.path.clone()) {
                    self.open_file(path, true);
                }
            }
            Action::OpenWith => {
                if let Some(path) = self.current().map(|e| e.path.clone()) {
                    self.open_prompt(PromptKind::OpenWith(path), "");
                }
            }
            Action::CopyPath => self.copy_paths(),
            Action::Shell => self.open_prompt(PromptKind::Shell, ""),
            Action::ShellHere => {
                if let Err(problem) = system::open_terminal(&self.cwd) {
                    self.status = problem;
                }

            }

            Action::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.reload();
                self.status = if self.show_hidden { "Showing hidden files" } else { "Hiding hidden files" }.into();
            }
            Action::Filter => {
                self.set_filter(String::new()); // every search starts fresh
                self.open_prompt(PromptKind::Filter, "");
            }
            Action::SortName => self.sort_by(SortBy::Name),
            Action::SortSize => self.sort_by(SortBy::Size),
            Action::SortModified => self.sort_by(SortBy::Modified),
            Action::ReverseSort => {
                self.reverse = !self.reverse;
                self.reload();
            }
            Action::Reload => {
                self.reload();
                self.status = "Reloaded".into();
            }

            Action::CancelJob => {
                if self.jobs.running > 0 {
                    self.jobs.cancel();
                    self.status = "Cancelling...".into();
                } else {
                    self.status = "No job is running".into();
                }
            }
            Action::Command => self.open_prompt(PromptKind::Command, ""),
            Action::Help => {
                self.help_scroll = 0;
                self.mode = Mode::Help;
            }
            Action::Quit => {
                if self.jobs.running > 0 {
                    self.status = "A job is still running. Wait for it, or press Ctrl-x to cancel it.".into();
                } else {
                    self.should_quit = true;
                }
            }
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, text: &str) {
        self.mode = Mode::Input(Prompt::new(kind, text));
    }

    fn toggle_visual(&mut self) {
        if matches!(self.mode, Mode::Visual { .. }) {
            self.mode = Mode::Normal; // leave visual mode, keep the selection
        } else if !self.entries.is_empty() {
            self.mode = Mode::Visual { anchor: self.cursor() };
            self.set_cursor(self.cursor()); // selects the item under the cursor
        }
    }

    fn clear_all(&mut self) {
        self.marked.clear();
        self.count = 0;
        self.mode = Mode::Normal;
        if !self.filter.is_empty() {
            self.set_filter(String::new());
        }
    }

    fn set_filter(&mut self, text: String) {
        self.filter = text;
        self.marked.clear();
        self.load_entries();
        self.set_cursor(0);
    }

    fn sort_by(&mut self, sort: SortBy) {
        self.sort = sort;
        self.reload();
        self.status = format!("Sorted by {}", sort.label());
    }

    fn start_jump(&mut self) {
        let here = self.cwd.clone();
        let folders: Vec<PathBuf> = self.store.ranked().into_iter().filter(|path| *path != here).collect();
        if folders.is_empty() {
            self.status = "No history yet. Visit some folders and they'll show up here".into();
            return;
        }
        self.mode = Mode::Picker(Picker::with_items("Jump to a folder", folders));
    }

    // ------------------------------------------------------ copy, paste, delete

    fn fill_clipboard(&mut self, cut: bool) {
        let paths = self.targets();
        if paths.is_empty() {
            return;
        }
        self.status = format!(
            "{} {}. Go to the destination and press p",
            if cut { "Cut" } else { "Copied" },
            plural(paths.len(), "item")
        );
        self.clipboard = Some(Clipboard { paths, cut });
        self.marked.clear();
        self.mode = Mode::Normal;
    }

    fn paste(&mut self) {
        let Some(clip) = self.clipboard.clone() else {
            self.status = "Nothing to paste. Copy (y) or cut (x) something first.".into();
            return;
        };
        if self.jobs.running > 0 {
            self.status = "Another job is running. Wait, or press Ctrl-x to cancel it".into();
            return;
        }

        let dest = self.cwd.clone();
        let clashes = clip
            .paths
            .iter()
            .filter(|path| {
                let Some(name) = path.file_name() else { return false };
                let target = dest.join(name);
                // Copying into the same folder just makes "name (1)", which is no clash.
                fsops::exists(&target) && target != **path
            })
            .count();

        if clashes > 0 {
            self.mode = Mode::Conflict(Conflict { clip, dest, clashes });
        } else {
            self.begin_transfer(clip, dest, OnConflict::Keep);
        }
    }

    fn begin_transfer(&mut self, clip: Clipboard, dest: PathBuf, on_conflict: OnConflict) {
        if clip.cut {
            self.clipboard = None; // a cut can only be pasted once
            self.start_job(Job::Move { items: clip.paths, dest, on_conflict });
        } else {
            self.start_job(Job::Copy { items: clip.paths, dest, on_conflict });
        }
    }

    fn start_job(&mut self, job: Job) {
        if self.jobs.running > 0 {
            self.status = "Another job is running. Wait, or press Ctrl-x to cancel it".into();
            return;
        }
        self.jobs.start(job);
    }

    fn ask_to_remove(&mut self, permanent: bool) {
        let items = self.targets();
        if items.is_empty() {
            return;
        }
        let what = describe(&items);
        let (question, job) = if permanent {
            (format!("Permanently delete {what}? This cannot be undone."), Job::Delete { items })
        } else {
            (format!("Move {what} to the trash? You can undo this."), Job::Trash { items })
        };
        self.mode = Mode::Confirm(Confirm { question, job });
    }

    // ------------------------------------------------------ rename, create, undo

    fn start_rename(&mut self) {
        if let Some(entry) = self.current() {
            let (path, name) = (entry.path.clone(), entry.name.clone());
            self.open_prompt(PromptKind::Rename(path), &name);
        }
    }

    fn rename(&mut self, path: &Path, new_name: &str) {
        if let Err(problem) = check_name(new_name) {
            self.status = problem.into();
            return;
        }
        let new_path = path.with_file_name(new_name);
        if new_path == path {
            return;
        }
        if fsops::exists(&new_path) {
            self.status = format!("{new_name} already exists");
            return;
        }

        match fs::rename(path, &new_path) {
            Ok(()) => {
                self.undo_stack.push(Undo::Moved(vec![(path.to_path_buf(), new_path)]));
                self.reload();
                self.select_name(new_name);
                self.status = format!("Renamed to {new_name}");
            }
            Err(e) => self.status = format!("Rename failed: {e}"),
        }
    }

    /// Opens the names of the selected items as a list in $EDITOR. Change the lines,
    /// save and quit, and everything is renamed. Swapping two names works too.
    fn start_bulk_rename(&mut self) {
        let items = self.targets();
        if items.is_empty() {
            return;
        }
        let names: Vec<String> =
            items.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        if names.iter().any(|n| n.contains('\n')) {
            self.status = "Can't bulk-rename a name that contains a line break".into();
            return;
        }

        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let file = env::temp_dir().join(format!("yatfm-rename-{}-{nanos}.txt", std::process::id()));
        let text = format!("{}\n", names.join("\n"));
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = written {
            self.status = format!("Couldn't create the temporary file: {e}");
            return;
        }

        let mut command = system::editor_words();
        command.push(file.to_string_lossy().into_owned());
        self.bulk = Some(BulkRename { file, items });
        self.handoff = Some(Handoff { command, wait: false });
    }

    fn finish_bulk_rename(&mut self, bulk: BulkRename) {
        let text = fs::read_to_string(&bulk.file);
        let _ = fs::remove_file(&bulk.file);
        let Ok(text) = text else {
            self.status = "Couldn't read the edited list".into();
            return;
        };

        let mut lines: Vec<&str> = text.lines().collect();
        while lines.last().map_or(false, |l| l.trim().is_empty()) {
            lines.pop();
        }
        if lines.len() != bulk.items.len() {
            self.status = format!("Cancelled: expected {} lines, found {}", bulk.items.len(), lines.len());
            return;
        }

        let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (path, new) in bulk.items.iter().zip(&lines) {
            let new = new.trim();
            let old = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if new == old {
                continue;
            }
            if let Err(problem) = check_name(new) {
                self.status = format!("Cancelled: \"{new}\": {problem}");
                return;
            }
            pairs.push((path.clone(), path.with_file_name(new)));
        }

        if pairs.is_empty() {
            self.status = "No names changed".into();
            return;
        }

        match fsops::rename_many(&pairs) {
            Ok(()) => {
                self.status = format!("Renamed {} (u to undo)", plural(pairs.len(), "item"));
                self.undo_stack.push(Undo::Moved(pairs));
            }
            Err(e) => self.status = format!("Bulk rename failed: {e}"),
        }
        self.marked.clear();
        self.reload();
    }

    fn create(&mut self, name: &str, is_folder: bool) {
        if let Err(problem) = check_name(name) {
            self.status = problem.into();
            return;
        }
        let path = self.cwd.join(name);
        if fsops::exists(&path) {
            self.status = format!("{name} already exists");
            return;
        }

        let result = if is_folder {
            fs::create_dir(&path)
        } else {
            fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ())
        };

        match result {
            Ok(()) => {
                self.undo_stack.push(Undo::Created(vec![path]));
                self.filter.clear(); // otherwise the new item might be hidden
                self.reload();
                self.select_name(name);
                self.status = format!("Created {name}");
            }
            Err(e) => self.status = format!("Couldn't create {name}: {e}"),
        }
    }

    fn undo(&mut self) {
        let Some(undo) = self.undo_stack.pop() else {
            self.status = "Nothing to undo".into();
            return;
        };
        self.status = match undo.apply() {
            Ok(message) => message,
            Err(problem) => format!("Undo failed: {problem}"),
        };
        self.reload();
    }

    // --------------------------------------------------- archives and the system

    fn start_compress(&mut self) {
        let paths = self.targets();
        if paths.is_empty() {
            return;
        }
        let base = match paths.as_slice() {
            [only] => only.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into()),
            _ => "archive".to_string(),
        };
        self.open_prompt(PromptKind::Compress, &format!("{base}.tar.gz"));
    }

    fn compress(&mut self, name: &str) {
        if let Err(problem) = check_name(name) {
            self.status = problem.into();
            return;
        }
        let items = self.targets();
        if items.is_empty() {
            return;
        }
        self.marked.clear();
        self.start_job(Job::Compress { items, dir: self.cwd.clone(), name: name.to_string() });
    }

    fn extract(&mut self) {
        let archives: Vec<PathBuf> = self
            .targets()
            .into_iter()
            .filter(|p| p.file_name().map_or(false, |n| archive::is_archive(&n.to_string_lossy())))
            .collect();
        if archives.is_empty() {
            self.status = "That isn't an archive I can extract (zip, tar.*, 7z, rar)".into();
            return;
        }
        self.marked.clear();
        self.start_job(Job::Extract { archives, dest: self.cwd.clone() });
    }

    fn copy_paths(&mut self) {
        let paths = self.targets();
        if paths.is_empty() {
            return;
        }
        let text = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        self.status = match system::copy_to_clipboard(&text) {
            Ok(()) if paths.len() == 1 => "Copied the path to the clipboard".into(),
            Ok(()) => format!("Copied {} paths to the clipboard", paths.len()),
            Err(e) => format!("Couldn't copy: {e}"),
        };
    }

    /// `%f` = the item under the cursor, `%s` = the selection (or the cursor item),
    /// `%d` = this folder, `%%` = a literal percent sign. All of them are quoted for you.
    fn run_shell(&mut self, template: &str) {
        if template.is_empty() {
            return;
        }
        let file = self.current().map(|e| system::shell_quote(&e.name)).unwrap_or_default();
        let selected: Vec<String> = self
            .targets()
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| system::shell_quote(&n.to_string_lossy()))
            .collect();
        let dir = system::shell_quote(&self.cwd.to_string_lossy());

        let line = expand(template, &file, &selected.join(" "), &dir);
        self.handoff = Some(Handoff { command: vec!["sh".into(), "-c".into(), line], wait: true });
    }

    fn open_with(&mut self, path: &Path, text: &str) {
        let mut command: Vec<String> = text.split_whitespace().map(String::from).collect();
        if command.is_empty() {
            return;
        }
        command.push(path.to_string_lossy().into_owned());
        if let Err(problem) = system::spawn_detached(&command) {
            self.status = problem;
        }
    }

    fn start_edit(&mut self) {
        match self.current() {
            Some(entry) if entry.is_file => {
                let mut command = system::editor_words();
                command.push(entry.path.to_string_lossy().into_owned());
                self.handoff = Some(Handoff { command, wait: false });
            }
            Some(_) => self.status = "Only files can be edited".into(),
            None => {}
        }
    }

    /// Called by `main` after the terminal comes back from an editor or shell.
    pub fn after_handoff(&mut self) {
        match self.bulk.take() {
            Some(bulk) => self.finish_bulk_rename(bulk),
            None => self.reload(),
        }
    }

    // ---------------------------------------------------------------- commands

    fn run_command(&mut self, line: &str) {
        let (command, arg) = match line.split_once(' ') {
            Some((command, arg)) => (command, arg.trim()),
            None => (line, ""),
        };

        match command {
            "" => {}
            "q" | "quit" => self.run(Action::Quit),
            "cd" => self.change_dir(arg),
            "mkdir" => self.create(arg, true),
            "touch" => self.create(arg, false),
            "rename" => {
                if let Some(path) = self.current().map(|e| e.path.clone()) {
                    self.rename(&path, arg);
                }
            }
            "hidden" => self.run(Action::ToggleHidden),
            "help" => self.run(Action::Help),
            "find" => self.run(Action::Find),
            "jump" => self.run(Action::Jump),
            "extract" => self.extract(),
            "compress" if arg.is_empty() => self.start_compress(),
            "compress" => self.compress(arg),
            "sort" => match arg {
                "name" => self.sort_by(SortBy::Name),
                "size" => self.sort_by(SortBy::Size),
                "modified" => self.sort_by(SortBy::Modified),
                _ => self.status = "Usage: sort name | size | modified".into(),
            },
            other => self.status = format!("Unknown command: {other}. Press ? for help."),
        }
    }

    fn change_dir(&mut self, arg: &str) {
        let path = match arg {
            "" | "~" => home(),
            _ => match arg.strip_prefix("~/") {
                Some(rest) => home().join(rest),
                None => self.cwd.join(arg), // absolute paths replace cwd automatically
            },
        };
        match fs::canonicalize(&path) {
            Ok(path) => self.go_to(path, None),
            Err(e) => self.status = format!("Can't go to {}: {e}", path.display()),
        }
    }

    // -------------------------------------------------------------------- jobs

    fn poll_jobs(&mut self) {
        let finished = self.jobs.poll();
        if finished.is_empty() {
            return;
        }
        for result in finished {
            match result {
                Ok(outcome) => {
                    self.status = outcome.message;
                    self.undo_stack.extend(outcome.undo);
                }
                Err(problem) => self.status = problem,
            }
        }
        self.reload();
    }
}

/// "report.pdf" or "14 items (a, b, c, ...)"
fn describe(paths: &[PathBuf]) -> String {
    let names: Vec<String> =
        paths.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();

    if names.len() == 1 {
        return names[0].clone();
    }
    let shown = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    let more = if names.len() > 3 { ", ..." } else { "" };
    format!("{} ({shown}{more})", plural(names.len(), "item"))
}

fn check_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        Err("Name can't be empty")
    } else if name.contains('/') {
        Err("Name can't contain /")
    } else if name == "." || name == ".." {
        Err("That name isn't allowed")
    } else {
        Ok(())
    }
}

fn expand_tilde(text: &str) -> PathBuf {
    if text == "~" {
        home()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(text)
    }
}

fn common_prefix(a: &str, b: &str) -> String {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x).collect()
}

/// Fill in %f %s %d in a single pass, so a file called "%s" can't expand twice.
fn expand(template: &str, file: &str, selected: &str, dir: &str) -> String {
    let mut out = String::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('f') => out.push_str(file),
            Some('s') => out.push_str(selected),
            Some('d') => out.push_str(dir),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}
