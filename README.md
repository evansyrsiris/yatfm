# yatfm

**The file manager with no bullshit.**

Yet Another Terminal File Manager. Vim keys if you want them, arrow keys and
on-screen hints if you don't. No config file needed, nothing to learn first:
press `?` and every key is listed.

## Dev setup (Arch Linux)

```sh
# 1. Toolchain and build tools
sudo pacman -S --needed base-devel git rustup xdg-utils
rustup default stable
rustup component add rust-analyzer clippy rustfmt

# 2. Handy dev tools (optional)
cargo install bacon          # re-runs clippy/tests every time you save

# 3. Build and run
cargo run                    # starts in the current folder
cargo run -- ~/Downloads     # or in a folder of your choice
cargo run --release          # optimized build: target/release/yatfm
```

`xdg-utils` provides `xdg-open`, which is what opens files in your default app.
Editors: `rust-analyzer` works out of the box with neovim, helix, VS Code, Zed...

Day to day:

```sh
cargo clippy                 # lints
cargo fmt                    # formatting (see rustfmt.toml)
cargo install --path .       # install to ~/.cargo/bin/yatfm
```

## Keys

Press `?` inside yatfm. The list is generated from `src/keymap.rs`.

| Do this              | Press                              |
| -------------------- | ---------------------------------- |
| Move                 | `h j k l` or arrow keys            |
| Open / enter         | `l`, `→` or `Enter`                |
| Select               | `Space`, or `v` then move          |
| Copy / cut / paste   | `y` / `x` / `p`                    |
| Trash (undoable)     | `d` then `y`                       |
| Undo                 | `u`                                |
| Rename / new         | `r` / `n` (file) / `N` (folder)    |
| Filter               | `/`                                |
| Hidden files         | `.`                                |
| Sort                 | `s` then `n` / `s` / `m` / `r`     |
| Command line         | `:`                                |

Press the first key of a multi-key binding (like `g` or `s`) and a popup lists
what you can press next.

## cd on quit

A program can't change its parent shell's folder, so yatfm writes its last
folder to a file and a tiny shell function does the `cd`.

bash / zsh:

```sh
y() {
  local tmp; tmp="$(mktemp)"
  yatfm --cwd-file "$tmp" "$@"
  [ -f "$tmp" ] && cd -- "$(cat "$tmp")"
  rm -f "$tmp"
}
```

fish:

```fish
function y
  set tmp (mktemp)
  yatfm --cwd-file $tmp $argv
  test -f $tmp; and cd (cat $tmp)
  rm -f $tmp
end
```

## Where things live

```
src/main.rs     start-up, the main loop, launching $EDITOR
src/app.rs      all program state + what every key and command does
src/keymap.rs   THE table of key bindings (help and which-key come from it)
src/ui.rs       drawing (reads the state, never changes it)
src/entry.rs    one file/folder entry + reading a folder
src/jobs.rs     copy/move/delete on a background thread
src/undo.rs     reversing changes (including restoring from the trash)
src/fsops.rs    plain filesystem helpers
src/preview.rs  the right-hand preview pane
src/format.rs   sizes, dates, permissions as text
```

Rules the code follows: no async runtime (a thread and a channel are enough),
no config before it's needed, no clever abstractions before a second use for them.

## Roadmap

- [ ] Mouse support (click, scroll, double-click)
- [ ] Syntax highlighting in the preview (`syntect`)
- [ ] Image preview (Kitty / Sixel)
- [ ] Sidebar: bookmarks and mounted drives (udisks2)
- [ ] Tabs
- [ ] Bulk rename in `$EDITOR`
- [ ] "Open with..." menu from mimeapps.list
- [ ] Properties / permissions dialog
- [ ] Recursive fuzzy search
- [ ] Optional `config.toml` for keys and colors
- [ ] Browse archives and SFTP through a virtual-filesystem trait
