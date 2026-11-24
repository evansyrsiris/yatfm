//! Every key binding lives in the one table below.
//!
//! The help screen and the "which key" popup are both generated from it,
//! so they can never disagree with what the keys actually do.
//! To rebind something, change its `keys` string. That's it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    // moving around
    Down,
    Up,
    Left,
    Right,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    Back,
    Forward,
    GoHome,
    GoRoot,
    GoDownloads,
    GoConfig,
    Jump,
    Find,
    SetBookmark,
    GoBookmark,
    // selecting
    Mark,
    Visual,
    SelectAll,
    ClearAll,
    // changing files
    Yank,
    Cut,
    Paste,
    Trash,
    Delete,
    Rename,
    BulkRename,
    NewFile,
    NewFolder,
    Undo,
    Edit,
    Compress,
    Extract,
    // opening and running things
    Open,
    OpenWith,
    CopyPath,
    Shell,
    ShellHere,
    // view
    ToggleHidden,
    Filter,
    SortName,
    SortSize,
    SortModified,
    ReverseSort,
    Reload,
    // other
    CancelJob,
    Command,
    Help,
    Quit,
}

pub struct Binding {
    /// Alternatives are separated by spaces. "gg" means press g twice.
    pub keys: &'static str,
    pub action: Action,
    pub desc: &'static str,
    pub group: &'static str,
}

const fn bind(group: &'static str, keys: &'static str, action: Action, desc: &'static str) -> Binding {
    Binding { keys, action, desc, group }
}

use Action::*;

pub const BINDINGS: &[Binding] = &[
    bind("Move", "j <Down>", Down, "Move down"),
    bind("Move", "k <Up>", Up, "Move up"),
    bind("Move", "h <Left>", Left, "Go to parent folder"),
    bind("Move", "l <Right> <Enter>", Right, "Open file / enter folder"),
    bind("Move", "gg <Home>", Top, "Jump to top"),
    bind("Move", "G <End>", Bottom, "Jump to bottom (12G = line 12)"),
    bind("Move", "<C-d>", HalfPageDown, "Half page down"),
    bind("Move", "<C-u>", HalfPageUp, "Half page up"),
    bind("Move", "H", Back, "Back in folder history"),
    bind("Move", "L", Forward, "Forward in folder history"),
    bind("Go to", "z", Jump, "Jump to a frequent folder (fuzzy)"),
    bind("Go to", "f <C-f>", Find, "Find a file below here (fuzzy)"),
    bind("Go to", "m", SetBookmark, "Bookmark this folder"),
    bind("Go to", "'", GoBookmark, "Go to a bookmark"),
    bind("Go to", "gh", GoHome, "Home folder"),
    bind("Go to", "gd", GoDownloads, "Downloads"),
    bind("Go to", "gc", GoConfig, "~/.config"),
    bind("Go to", "g/", GoRoot, "Root folder /"),
    bind("Select", "<Space>", Mark, "Select / unselect"),
    bind("Select", "v", Visual, "Select a range"),
    bind("Select", "<C-a>", SelectAll, "Select everything"),
    bind("Select", "<Esc>", ClearAll, "Clear selection and filter"),
    bind("Files", "y", Yank, "Copy"),
    bind("Files", "x", Cut, "Cut (to move)"),
    bind("Files", "p", Paste, "Paste here"),
    bind("Files", "d", Trash, "Move to trash"),
    bind("Files", "D", Delete, "Delete permanently"),
    bind("Files", "r", Rename, "Rename"),
    bind("Files", "R", BulkRename, "Rename many in $EDITOR"),
    bind("Files", "n", NewFile, "New file"),
    bind("Files", "N", NewFolder, "New folder"),
    bind("Files", "u", Undo, "Undo last change"),
    bind("Files", "C", Compress, "Compress to zip / tar.gz / ..."),
    bind("Files", "E", Extract, "Extract archive here"),
    bind("Run", "e", Edit, "Edit in $EDITOR"),
    bind("Run", "o", Open, "Open with the system default"),
    bind("Run", "O", OpenWith, "Open with a GUI app..."),
    bind("Run", "Y", CopyPath, "Copy full path to clipboard"),
    bind("Run", "!", Shell, "Run a shell command"),
    bind("Run", "S", ShellHere, "Open a new terminal in this directory."),
    bind("View", ".", ToggleHidden, "Show / hide hidden files"),
    bind("View", "/", Filter, "Filter this folder"),
    bind("View", "sn", SortName, "Sort by name"),
    bind("View", "ss", SortSize, "Sort by size"),
    bind("View", "sm", SortModified, "Sort by date modified"),
    bind("View", "sr", ReverseSort, "Reverse sort order"),
    bind("View", "<C-r>", Reload, "Reload"),
    bind("Other", "<C-x>", CancelJob, "Cancel the running job"),
    bind("Other", ":", Command, "Command line"),
    bind("Other", "?", Help, "This help"),
    bind("Other", "q <C-c>", Quit, "Quit"),
];

/// Turn a key press into the text used in the table above: "j", "G", "<C-d>", "<Esc>"...
pub fn key_name(key: KeyEvent) -> Option<String> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let name = match key.code {
        KeyCode::Char(c) if ctrl => format!("<C-{c}>"),
        KeyCode::Char(' ') => "<Space>".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "<Enter>".into(),
        KeyCode::Esc => "<Esc>".into(),
        KeyCode::Up => "<Up>".into(),
        KeyCode::Down => "<Down>".into(),
        KeyCode::Left => "<Left>".into(),
        KeyCode::Right => "<Right>".into(),
        KeyCode::Home => "<Home>".into(),
        KeyCode::End => "<End>".into(),
        _ => return None,
    };
    Some(name)
}

pub enum Lookup {
    /// A complete binding. Run it.
    Run(Action),
    /// The start of a longer binding, like the first "g" of "gg". Wait for more keys.
    Pending,
    /// Nothing starts with that.
    Unknown,
}

pub fn lookup(typed: &str) -> Lookup {
    let mut pending = false;
    for binding in BINDINGS {
        for keys in binding.keys.split_whitespace() {
            if keys == typed {
                return Lookup::Run(binding.action);
            }
            if keys.starts_with(typed) {
                pending = true;
            }
        }
    }
    if pending {
        Lookup::Pending
    } else {
        Lookup::Unknown
    }
}

/// For the "which key" popup: everything you could press next, e.g. after "g".
pub fn continuations(typed: &str) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    for binding in BINDINGS {
        for keys in binding.keys.split_whitespace() {
            if keys.len() > typed.len() && keys.starts_with(typed) {
                out.push((&keys[typed.len()..], binding.desc));
            }
        }
    }
    out
}
