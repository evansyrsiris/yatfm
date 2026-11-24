//! YATFM: the file manager with no bullshit.

mod app;
mod archive;
mod entry;
mod format;
mod fsops;
mod jobs;
mod keymap;
mod picker;
mod preview;
mod store;
mod system;
mod ui;
mod undo;

use std::{env, fs, path::{Path, PathBuf}, process::Command, time::Duration};

use anyhow::{Context, Result};
use ratatui::{
    crossterm::event::{self, Event, KeyEventKind},
    DefaultTerminal,
};

use app::{App, Handoff};

const USAGE: &str = "\
yatfm: the file manager with no bullshit.

Usage: yatfm [options] [folder]

Options:
  --cwd-file <file>   On exit, write the last folder to <file>
                      (lets your shell cd there, see README)
  -h, --help          Show this help
  -V, --version       Show the version

Press ? inside yatfm to see every key.
Optional settings: ~/.config/yatfm/config";

fn main() -> Result<()> {
    let mut start: Option<PathBuf> = None;
    let mut cwd_file: Option<PathBuf> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(println!("{USAGE}")),
            "-V" | "--version" => return Ok(println!("yatfm {}", env!("CARGO_PKG_VERSION"))),
            "--cwd-file" => cwd_file = Some(args.next().context("--cwd-file needs a file name")?.into()),
            path => start = Some(PathBuf::from(path)),
        }
    }

    let start = fs::canonicalize(start.unwrap_or_else(|| PathBuf::from("."))).context("that folder doesn't exist")?;
    if !start.is_dir() {
        anyhow::bail!("{} is not a folder", start.display());
    }

    let mut app = App::new(start);
    let mut terminal = ratatui::init(); // also restores the terminal if we crash
    let result = run(&mut terminal, &mut app);
    ratatui::restore();

    app.save_state(); // bookmarks and visited folders

    if let Some(file) = cwd_file {
        fs::write(file, app.cwd.as_os_str().as_encoded_bytes())?;
    }
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Wait a moment for a key. Then handle every key that's already waiting before
        // drawing again, so holding a key down never makes the screen lag behind.
        if event::poll(Duration::from_millis(30))? {
            loop {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        app.on_key(key);
                    }
                }
                if app.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        app.tick();

        if let Some(handoff) = app.handoff.take() {
            if let Err(problem) = hand_over(terminal, &handoff, &app.cwd) {
                app.status = format!("{problem}");
            }
            app.after_handoff();
        }
    }
    Ok(())
}

/// Give the terminal to another program (editor, shell, a command), then take it back.
fn hand_over(terminal: &mut DefaultTerminal, handoff: &Handoff, cwd: &Path) -> Result<()> {
    let (program, args) = handoff.command.split_first().context("nothing to run")?;

    ratatui::restore();
    let result = Command::new(program).args(args).current_dir(cwd).status();
    if handoff.wait && result.is_ok() {
        println!("\n[Done. Press Enter to go back to yatfm]");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    *terminal = ratatui::init();

    result.with_context(|| format!("couldn't run {program}"))?;
    Ok(())
}
