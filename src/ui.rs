//! Drawing. This file only reads the `App`; it never changes what the program does.

use std::{collections::HashSet, path::PathBuf};

use ratatui::{
    layout::{Alignment, Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{App, BookmarkMode, Conflict, Mode, Prompt, PromptKind},
    entry::Entry,
    format::{ago, human_size, mode_string, plural},
    keymap::{self, BINDINGS},
    picker::{Item, Picker},
    preview::Preview,
};

const DIM: Style = Style::new().fg(Color::DarkGray);
const FOLDER: Style = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
const LINK: Style = Style::new().fg(Color::Cyan);
const RUNNABLE: Style = Style::new().fg(Color::Green);
const MARK: Style = Style::new().fg(Color::Yellow);
const KEY: Style = Style::new().fg(Color::Yellow);
const HEADING: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const CURSOR: Style = Style::new().add_modifier(Modifier::REVERSED);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let [left, middle, right] =
        Layout::horizontal([Constraint::Percentage(20), Constraint::Percentage(40), Constraint::Percentage(40)])
            .areas(body);

    app.page = body.height.saturating_sub(2) as usize;

    draw_header(frame, app, header);
    draw_parent(frame, app, left);
    draw_current(frame, app, middle);
    draw_preview(frame, app, right);
    draw_footer(frame, app, footer);

    // Popups go on top of everything else.
    match &app.mode {
        Mode::Help => draw_help(frame, app, body),
        Mode::Confirm(confirm) => draw_confirm(frame, &confirm.question, body),
        Mode::Conflict(conflict) => draw_conflict(frame, conflict, body),
        Mode::Bookmark(kind) => draw_bookmarks(frame, app, *kind, body),
        Mode::Picker(picker) => draw_picker(frame, picker, body),
        Mode::Input(prompt) => draw_hint(frame, prompt, body),
        _ => {}
    }
    if !app.typed.is_empty() {
        draw_which_key(frame, &app.typed, body);
    }
}

// ----------------------------------------------------------------------- panes

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(app.cwd.display().to_string(), MARK.add_modifier(Modifier::BOLD))];
    if !app.filter.is_empty() {
        spans.push(Span::styled(format!("   filter: {}", app.filter), Style::new().fg(Color::Magenta)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn pane(title: &str) -> Block<'_> {
    Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(DIM).title(title)
}

fn draw_parent(frame: &mut Frame, app: &App, area: Rect) {
    let block = pane("");
    let width = block.inner(area).width as usize;
    let items = entry_items(&app.parent_entries, &HashSet::new(), width, false);

    let mut state = ListState::default().with_selected(app.parent_selected);
    frame.render_stateful_widget(List::new(items).block(block).highlight_style(CURSOR), area, &mut state);
}

fn draw_current(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = pane("").border_style(Style::new().fg(Color::Blue));
    let width = block.inner(area).width as usize;

    if app.entries.is_empty() {
        let text = if app.filter.is_empty() { "(empty folder)" } else { "(nothing matches the filter)" };
        frame.render_widget(Paragraph::new(Span::styled(text, DIM)).block(block), area);
        return;
    }

    let items = entry_items(&app.entries, &app.marked, width, true);
    frame.render_stateful_widget(List::new(items).block(block).highlight_style(CURSOR), area, &mut app.list);
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let title = app.current().map(|e| format!(" {} ", e.name)).unwrap_or_default();
    let block = pane(&title);
    let width = block.inner(area).width as usize;

    // While a slow preview loads, show an empty pane rather than the wrong file's contents.
    if app.preview_loading() {
        frame.render_widget(block, area);
        return;
    }

    match &app.preview {
        Preview::Empty => frame.render_widget(block, area),
        Preview::Note(text) => frame.render_widget(Paragraph::new(Span::styled(text.as_str(), DIM)).block(block), area),
        Preview::Text(lines) => {
            let lines: Vec<Line> = lines.iter().map(|l| Line::raw(l.as_str())).collect();
            frame.render_widget(Paragraph::new(lines).block(block), area);
        }
        Preview::Folder(entries) => {
            let items = entry_items(entries, &HashSet::new(), width, false);
            frame.render_widget(List::new(items).block(block), area);
        }
    }
}

fn entry_items(entries: &[Entry], marked: &HashSet<PathBuf>, width: usize, show_size: bool) -> Vec<ListItem<'static>> {
    entries.iter().map(|e| ListItem::new(entry_line(e, marked.contains(&e.path), width, show_size))).collect()
}

/// `● name/                 4.2 KB`
fn entry_line(entry: &Entry, marked: bool, width: usize, show_size: bool) -> Line<'static> {
    let style = if entry.is_dir {
        FOLDER
    } else if entry.is_symlink {
        LINK
    } else if entry.is_executable() {
        RUNNABLE
    } else {
        Style::new()
    };

    let name = if entry.is_dir { format!("{}/", entry.name) } else { entry.name.clone() };
    let size = if show_size && !entry.is_dir { human_size(entry.size) } else { String::new() };

    let marker_width = 2;
    let room = width.saturating_sub(marker_width + size.chars().count() + 1);
    let name = truncate(&name, room);
    let padding = width.saturating_sub(marker_width + name.chars().count() + size.chars().count());

    Line::from(vec![
        Span::styled(if marked { "● " } else { "  " }, MARK),
        Span::styled(name, style),
        Span::raw(" ".repeat(padding)),
        Span::styled(size, DIM),
    ])
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let kept: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

// ---------------------------------------------------------------------- footer

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    // While typing, the footer is the text box.
    if let Mode::Input(prompt) = &app.mode {
        let label = match &prompt.kind {
            PromptKind::Rename(_) => "Rename: ",
            PromptKind::NewFile => "New file: ",
            PromptKind::NewFolder => "New folder: ",
            PromptKind::Filter => "/",
            PromptKind::Command => ":",
            PromptKind::Shell => "!",
            PromptKind::OpenWith(_) => "Open with: ",
            PromptKind::Compress => "Compress to: ",
        };
        let line = Line::from(vec![Span::styled(label, MARK), Span::raw(prompt.text.as_str())]);
        let cursor_x = area.x + (label.chars().count() + prompt.cursor) as u16;
        frame.render_widget(Paragraph::new(line), area);
        frame.set_cursor_position(Position::new(cursor_x.min(area.right().saturating_sub(1)), area.y));
        return;
    }

    let left = if let Some(progress) = app.jobs.status() {
        format!("⏳ {progress}")
    } else if !app.status.is_empty() {
        app.status.clone()
    } else {
        info_line(app)
    };
    frame.render_widget(Paragraph::new(left), area);
    frame.render_widget(Paragraph::new(Span::styled("? help  : command ", DIM)).alignment(Alignment::Right), area);
}

/// What's shown when nothing else needs saying.
fn info_line(app: &App) -> String {
    let mut parts = Vec::new();

    if app.count > 0 {
        parts.push(format!("{}…", app.count));
    }
    if matches!(app.mode, Mode::Visual { .. }) {
        parts.push("-- VISUAL --".to_string());
    }
    if let Some(entry) = app.current() {
        parts.push(mode_string(entry));
        if !entry.is_dir {
            parts.push(human_size(entry.size));
        }
        parts.push(ago(entry.modified));
    }
    parts.push(format!("{} · by {}", plural(app.entries.len(), "item"), app.sort.label()));
    if !app.marked.is_empty() {
        parts.push(format!("{} selected", app.marked.len()));
    }
    if let Some(clip) = &app.clipboard {
        parts.push(format!("{} {}", plural(clip.paths.len(), "item"), if clip.cut { "cut" } else { "copied" }));
    }
    parts.join("  ·  ")
}

// ---------------------------------------------------------------------- popups

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height)
}

fn popup_block(title: &str) -> Block<'_> {
    Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(MARK).title(title)
}

fn draw_confirm(frame: &mut Frame, question: &str, area: Rect) {
    let popup = centered(area, 60, 6);
    let text = vec![
        Line::raw(question),
        Line::raw(""),
        Line::from(vec![
            Span::styled("y", KEY),
            Span::raw(" yes     "),
            Span::styled("any other key", KEY),
            Span::raw(" cancel"),
        ]),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).block(popup_block(" Are you sure? ")), popup);
}

/// Pasting would collide with names that already exist here.
fn draw_conflict(frame: &mut Frame, conflict: &Conflict, area: Rect) {
    let popup = centered(area, 60, 8);
    let text = vec![
        Line::raw(format!("{} with the same name already here.", plural(conflict.clashes, "item"))),
        Line::raw(""),
        Line::from(vec![Span::styled("k", KEY), Span::raw("  keep both (the new one becomes \"name (1)\")")]),
        Line::from(vec![Span::styled("o", KEY), Span::raw("  overwrite (the old one goes to the trash)")]),
        Line::from(vec![Span::styled("s", KEY), Span::raw("  skip those")]),
        Line::from(vec![Span::styled("any other key", KEY), Span::raw("  cancel")]),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(text).block(popup_block(" Already exists ")), popup);
}

fn draw_bookmarks(frame: &mut Frame, app: &App, kind: BookmarkMode, area: Rect) {
    let title = match kind {
        BookmarkMode::Set => " Bookmark this folder as... (press a key) ",
        BookmarkMode::Go => " Go to bookmark (press its key) ",
    };

    let mut lines: Vec<Line> = app
        .store
        .bookmarks
        .iter()
        .map(|(key, path)| Line::from(vec![Span::styled(format!("{key}  "), KEY), Span::raw(path.display().to_string())]))
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled("(none yet: press m, then a key, to add one)", DIM));
    }

    let height = lines.len() as u16 + 2;
    let popup = centered(area, 70, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(popup_block(title)), popup);
}

/// The fuzzy list: a text box on top, matches below.
fn draw_picker(frame: &mut Frame, picker: &Picker, area: Rect) {
    let popup = centered(area, (area.width * 8 / 10).max(40), (area.height * 8 / 10).max(10));
    let title = format!(" {} ", picker.title);
    let bottom = format!(
        " {}/{}{} · ↑↓ choose · Enter go · Esc close ",
        picker.shown.len(),
        picker.items.len(),
        if picker.loading { " · searching..." } else { "" }
    );
    let block = popup_block(&title).title_bottom(Line::styled(bottom, DIM));
    let inner = block.inner(popup);

    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let [query_area, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);

    let query = Line::from(vec![Span::styled("> ", KEY), Span::raw(picker.query.as_str())]);
    frame.render_widget(Paragraph::new(query), query_area);
    let cursor_x = query_area.x + 2 + picker.query.chars().count() as u16;
    frame.set_cursor_position(Position::new(cursor_x.min(query_area.right().saturating_sub(1)), query_area.y));

    if picker.shown.is_empty() {
        let text = if picker.loading { "searching..." } else { "(no matches)" };
        frame.render_widget(Paragraph::new(Span::styled(text, DIM)), list_area);
        return;
    }

    let items: Vec<ListItem> = picker.shown.iter().map(|&i| ListItem::new(picker_line(&picker.items[i]))).collect();
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(List::new(items).highlight_style(CURSOR), list_area, &mut state);
}

/// The folder part dimmed, the name itself bright.
fn picker_line(item: &Item) -> Line<'static> {
    let (dir, name) = match item.label.trim_end_matches('/').rfind('/') {
        Some(i) => (&item.label[..=i], &item.label[i + 1..]),
        None => ("", item.label.as_str()),
    };
    let name_style = if item.is_dir { FOLDER } else { Style::new() };
    Line::from(vec![Span::styled(dir.to_string(), DIM), Span::styled(name.to_string(), name_style)])
}

/// Tab-completion matches, shown on the line above the prompt.
fn draw_hint(frame: &mut Frame, prompt: &Prompt, area: Rect) {
    if prompt.hint.is_empty() {
        return;
    }
    let row = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
    frame.render_widget(Clear, row);
    frame.render_widget(Paragraph::new(Span::styled(prompt.hint.as_str(), DIM)), row);
}

/// Shown after you press a key that needs a second key, like `g`.
fn draw_which_key(frame: &mut Frame, typed: &str, area: Rect) {
    let options = keymap::continuations(typed);
    if options.is_empty() {
        return;
    }
    let height = options.len() as u16 + 2;
    let width = 38;
    let popup = Rect::new(
        area.right().saturating_sub(width + 1),
        area.bottom().saturating_sub(height),
        width.min(area.width),
        height.min(area.height),
    );

    let lines: Vec<Line> = options
        .into_iter()
        .map(|(key, desc)| Line::from(vec![Span::styled(format!("{key:<6}"), KEY), Span::raw(desc)]))
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(popup_block(&format!(" {typed} "))), popup);
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered(area, 72, area.height);
    let block = popup_block(" Keys ").title_bottom(Line::styled(" j/k scroll · any other key closes ", DIM));

    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(help_lines()).scroll((app.help_scroll, 0)).block(block), popup);
}

fn help_lines() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut group = "";

    for binding in BINDINGS {
        if binding.group != group {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(binding.group, HEADING));
            group = binding.group;
        }
        lines.push(Line::from(vec![Span::styled(format!("  {:<20}", binding.keys), KEY), Span::raw(binding.desc)]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Numbers", HEADING));
    lines.push(Line::raw("  5j moves five down, 3<Space> selects three, 12G jumps to line 12"));

    lines.push(Line::raw(""));
    lines.push(Line::styled("Commands (press : first, Tab completes folders after cd)", HEADING));
    for (command, desc) in [
        ("cd <path>", "Go to a folder"),
        ("mkdir <name>", "New folder"),
        ("touch <name>", "New file"),
        ("rename <name>", "Rename the current item"),
        ("find", "Fuzzy-find a file below here"),
        ("jump", "Jump to a frequent folder"),
        ("compress <name.zip>", "Also .tar.gz .tar.xz .tar.zst .7z"),
        ("extract", "Extract the selected archive(s)"),
        ("sort name|size|modified", "Change sort order"),
        ("hidden", "Show / hide hidden files"),
        ("q", "Quit"),
    ] {
        lines.push(Line::from(vec![Span::styled(format!("  {command:<26}"), KEY), Span::raw(desc)]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Shell commands (press !)", HEADING));
    lines.push(Line::raw("  %f the item under the cursor, %s the selection, %d this folder"));
    lines.push(Line::raw("  (all quoted for you; write %% for a literal %)"));
    lines.push(Line::raw("  Example:  !convert %f out.png"));
    lines
}
