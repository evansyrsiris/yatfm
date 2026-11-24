//! Small helpers that talk to the rest of the system: clipboard, shell quoting, editor lookup.

use std::{
    env,
    fs::File,
    io::{self, Read, Write},
    path::Path,
    process::{Command, Stdio},
};

/// `it's` becomes `'it'\''s'`, safe to paste into `sh -c`.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// $VISUAL or $EDITOR, split into words so EDITOR="code --wait" works. Falls back to vi.
pub fn editor_words() -> Vec<String> {
    let editor = env::var("VISUAL")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .or_else(|| env::var("EDITOR").ok().filter(|e| !e.trim().is_empty()))
        .unwrap_or_else(|| "vi".into());
    editor.split_whitespace().map(String::from).collect()
}

pub fn shell_program() -> String {
    env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "sh".into())
}

/// Cheap check used before opening a file in the editor: no NUL bytes in the first 4 KB.
pub fn looks_like_text(path: &Path) -> bool {
    let mut buffer = [0u8; 4096];
    match File::open(path).and_then(|mut file| file.read(&mut buffer)) {
        Ok(read) => !buffer[..read].contains(&0),
        Err(_) => false,
    }
}

/// Start a program (a window, usually) and don't wait for it.
pub fn spawn_detached(command: &[String]) -> Result<(), String> {
    let (program, args) = command.split_first().ok_or("nothing to run")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't run {program}: {e}"))?;
    // Reap it when it exits, so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Put text on the system clipboard: Wayland, then X11, then the terminal's own
/// clipboard (OSC 52, which also works over SSH in most modern terminals).
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let tools: [(&str, &[&str]); 3] =
        [("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])];

    for (program, args) in tools {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().map(|status| status.success()).unwrap_or(false) {
            return Ok(());
        }
    }

    let mut out = io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes())).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = ((chunk[0] as u32) << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Open a new terminal window in `dir`. Uses $TERMINAL if set, otherwise the first
/// common terminal it finds.
pub fn open_terminal(dir: &Path) -> Result<(), String> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(terminal) = env::var("TERMINAL") {
        if !terminal.trim().is_empty() {
            candidates.push(terminal);
        }
    }
    for name in [
        "x-terminal-emulator",
        "kitty",
        "alacritty",
        "wezterm",
        "foot",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "xterm",
    ] {
        candidates.push(name.to_string());
    }

    for candidate in candidates {
        let mut words = candidate.split_whitespace();
        let Some(program) = words.next() else {
            continue;
        };

        // Most terminals start in the folder we launch them from. These need to be told.
        let extra: Vec<String> = match program {
            "gnome-terminal" | "xfce4-terminal" => vec![format!("--working-directory={}", dir.display())],
            "konsole" => vec!["--workdir".to_string(), dir.display().to_string()],
            _ => Vec::new(),
        };

        let spawned = Command::new(program)
            .args(words)
            .args(&extra)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("couldn't start {program}: {e}")),
        }
    }
    Err("no terminal found. Set the TERMINAL environment variable, e.g. TERMINAL=kitty".to_string())
}
