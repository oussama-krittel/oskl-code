# kite

A lightweight, git-first code editor for macOS, built in Rust on GPUI (Zed's GPU UI framework).

## Run

```bash
cargo build --release
./target/release/kite /path/to/project
```

The release build uses about 100 MB of memory and ships as a 6 MB binary.

GPUI compiles its Metal shaders when the app starts (the `runtime_shaders` feature), so no Metal toolchain download is needed. To precompile them instead, run `xcodebuild -downloadComponent MetalToolchain` and drop that feature in `Cargo.toml`.

## What's in it

- **Source Control panel:** commit box (⌘Enter), staged and unstaged changes grouped by folder, stage/unstage/discard on hover or by keyboard, amend toggle, and a lane graph with branch, remote and tag badges.
- **Commit view:** click a commit to see its hash, author, date, parents, message and changed files. Click a file to see its diff.
- **Diff view:** side-by-side or inline, with word-level highlights. Blame on both sides (`b`); clicking a blame cell opens that commit.
- **Editor:** inline blame for the current line, a blame gutter (⌥⌘B) and change markers against the index. Keys are VS Code plus readline/emacs. Mouse: click, drag, double-click word, triple-click line, line-number select, scrollbar drag, trackpad scrolling.
- **Explorer:** git status colors, and ignored files are dimmed.
- **Command line:** `:` / ⌘⇧P, file finder ⌘P, search ⌘F. Commands include `w`, `q`, `e`, `new`, `mv`, `format`, `lint`, `blame`, `diff`, `stage`, `commit`, `checkout`, `push`, `pull`, `git …` and `!shell`. Tab completes paths and branches.
- **Format and lint:** run as external CLIs. Biome or Prettier for JS/TS, with ESLint/Biome lint, plus rustfmt, gofmt, ruff and others. Output lines with locations are clickable.

- **Search & replace across files (⇧⌘F):** match case / whole word / regex, include/exclude globs, dismiss single results or whole files (dismissed ones are never replaced), replace one result, one file, or all. Regex replacements support `$1`.
- **Editable diffs:** unstaged changes open as Index ↔ Working Tree with an editable right side (⌘S saves), side-by-side or inline (⌘K I), F7 / ⇧F7 between changes.
- **Folders:** ⌘O picker, `:open <dir>`, `:recent`, recent list on the welcome screen.
- **Layout:** sidebar on the left or right (title bar button, ⌘K ⌘B, `:sidebar right`). Settings live in `~/.config/kite/settings`.

- **Panes instead of tabs (zellij / iTerm style):** ⌘D / ⌘⇧D split, ⌘⌥ arrows or ⌃⌥ H/J/K/L move between panes, drag gaps to resize, ⌘K Z maximizes, ⌘⇧W closes. Each pane keeps its own files (⌘E lists them, ⌃Tab cycles).
- **Terminal:** a real terminal using alacritty_terminal. ⌃` jumps to it or opens one at the bottom, ⌘T opens one in the focused pane, ⌘-click opens a `file:line` it printed. vim, htop and less work, including mouse and scrolling.
- **Auto save:** on by default, 1 second after you stop typing. Toggle it from the status bar or with `:autosave off`.

Every shortcut is listed in the ⌨ sidebar tab (⌘K ⌘S), filterable; `:keys` prints the same list.
