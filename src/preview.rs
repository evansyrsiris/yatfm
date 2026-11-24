//! What to show in the right-hand pane.
//!
//! Previews are loaded by a background thread. When you hold `j` and fly down a
//! folder, requests pile up and the thread only does the newest one.

use std::{
    fs::File,
    io::Read,
    sync::mpsc::{channel, Receiver, Sender},
    thread,
};

use crate::{
    entry::{self, Entry, SortBy},
    format::human_size,
};

pub enum Preview {
    Empty,
    Text(Vec<String>),
    Folder(Vec<Entry>),
    Note(String),
}

const MAX_BYTES: usize = 32 * 1024;
const MAX_LINES: usize = 200;
const MAX_FOLDER_ROWS: usize = 500;

impl Preview {
    pub fn load(entry: Option<&Entry>, show_hidden: bool) -> Preview {
        let Some(entry) = entry else {
            return Preview::Empty;
        };

        if entry.is_dir {
            return match entry::read_dir(&entry.path, show_hidden, SortBy::Name, false) {
                Ok(list) if list.is_empty() => Preview::Note("(empty folder)".into()),
                Ok(mut list) => {
                    list.truncate(MAX_FOLDER_ROWS);
                    Preview::Folder(list)
                }
                Err(e) => Preview::Note(format!("Can't read folder: {e}")),
            };
        }
        // Never open pipes, sockets or devices: reading them can hang forever.
        if !entry.is_file {
            return Preview::Note("(special file)".into());
        }
        if entry.size == 0 {
            return Preview::Note("(empty file)".into());
        }

        let mut buffer = vec![0; MAX_BYTES];
        let read = File::open(&entry.path).and_then(|mut file| file.read(&mut buffer));
        let Ok(read) = read else {
            return Preview::Note("Can't read file".into());
        };
        buffer.truncate(read);

        if buffer.contains(&0) {
            return Preview::Note(format!("Binary file, {}", human_size(entry.size)));
        }

        let lines = String::from_utf8_lossy(&buffer)
            .lines()
            .take(MAX_LINES)
            .map(|line| line.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect())
            .collect();
        Preview::Text(lines)
    }
}

struct Request {
    id: u64,
    entry: Entry,
    show_hidden: bool,
}

/// Owns the background thread. Ask with `request`, collect with `poll`.
pub struct PreviewLoader {
    requests: Sender<Request>,
    results: Receiver<(u64, Preview)>,
}

impl PreviewLoader {
    pub fn new() -> Self {
        let (requests, request_rx) = channel::<Request>();
        let (result_tx, results) = channel::<(u64, Preview)>();

        thread::spawn(move || {
            while let Ok(mut request) = request_rx.recv() {
                // Skip anything that's already out of date.
                while let Ok(newer) = request_rx.try_recv() {
                    request = newer;
                }
                let preview = Preview::load(Some(&request.entry), request.show_hidden);
                if result_tx.send((request.id, preview)).is_err() {
                    return; // the app is gone
                }
            }
        });

        PreviewLoader { requests, results }
    }

    pub fn request(&self, id: u64, entry: Entry, show_hidden: bool) {
        let _ = self.requests.send(Request { id, entry, show_hidden });
    }

    /// The newest finished preview, if any.
    pub fn poll(&self) -> Option<(u64, Preview)> {
        let mut latest = None;
        while let Ok(result) = self.results.try_recv() {
            latest = Some(result);
        }
        latest
    }
}
