//! The window's shell: the slim title bar, the breadcrumb under a file's tabs, the start screen
//! of an empty pane or split, zen mode and the hints that follow ⌘K. A child of `workspace`, so
//! it works on the same private state.

use super::*;

/// The title bar's height, a little past the window buttons.
pub(super) const TITLE_H: f32 = 30.;
/// Below this width (interface pixels) the sidebar lies over the editor instead of beside it.
const OVERLAY_W: f32 = 860.;
/// Below this width side-by-side splits stack.
const STACK_W: f32 = 640.;
/// How long ⌘K waits before listing what can follow it, so a quick chord stays quiet.
const CHORD_DELAY_MS: u64 = 450;

#[derive(Default)]
pub(super) struct Shell {
    /// Only the focused editor shows.
    pub zen: bool,
    /// The note that says how to leave zen mode, while it is up.
    zen_note: Option<Task<()>>,
    /// The window is narrow enough for the sidebar to lie over the editor / for splits to stack.
    pub overlay: bool,
    pub stack: bool,
    /// The project switcher is open.
    pub project_menu: bool,
    /// The bottom panel shows the problems instead of a command's output.
    pub problems: bool,
    /// ⌘K has waited long enough for its hints to show.
    chord_ready: bool,
    chord_task: Option<Task<()>>,
    /// The title bar's tint (0xRRGGBB), saved with the project.
    pub color: Option<u32>,
    /// Custom… in the project menu: the hex field, and the color to go back to on Esc.
    pub color_input: Option<(Entity<Editor>, Option<u32>, Subscription)>,
}

/// Window colors to pick from: mid tones that stay soft once mixed into the title bar.
pub(super) const WINDOW_COLORS: [(&str, u32); 8] = [
    ("blue", 0x5b7db1),
    ("teal", 0x3f8f8a),
    ("green", 0x5f8f4e),
    ("amber", 0xb08a3e),
    ("clay", 0xb5674d),
    ("rose", 0xa65d7a),
    ("violet", 0x7c6bb0),
    ("gray", 0x6b7280),
];

/// How strongly the color shows in the title bar: enough to spot the window, not enough to tire the eye.
const TINT: f32 = 0.5;

/// "#3f8f8a", "3f8f8a", "#38a" or a preset's name → 0xRRGGBB.
pub(super) fn parse_window_color(text: &str) -> Option<u32> {
    let t = text.trim().to_ascii_lowercase();
    if let Some((_, c)) = WINDOW_COLORS.iter().find(|(n, _)| *n == t) {
        return Some(*c);
    }
    let hex = t.strip_prefix('#').unwrap_or(&t);
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => u32::from_str_radix(&hex.chars().flat_map(|c| [c, c]).collect::<String>(), 16).ok(),
        _ => None,
    }
}

fn kbd(text: impl Into<SharedString>) -> Div {
    div().flex_none().px(px(4.)).rounded(px(3.)).bg(theme::badge()).text_color(theme::fg_bright()).text_size(px(11.)).child(text.into())
}

fn shortcut(name: &str, fallback: &str) -> String {
    crate::keybindings::shortcut_label(name).unwrap_or_else(|| fallback.to_string())
}

impl Workspace {
    pub(super) fn measure_shell(&mut self, window: &Window) {
        let w = window.viewport_size().width / gpui::px(1.) / ui::scale();
        self.shell.overlay = w < OVERLAY_W;
        self.shell.stack = w < STACK_W;
    }

    // ------------------------------------------------------------------ zen mode

    pub(super) fn toggle_zen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_zen(!self.shell.zen, window, cx);
    }

    pub(super) fn set_zen(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.zen = on;
        self.shell.project_menu = false;
        self.shell.zen_note = on.then(|| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                this.update(cx, |ws, cx| {
                    ws.shell.zen_note = None;
                    cx.notify();
                })
                .ok();
            })
        });
        // Whatever had the keyboard may have just gone from view.
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Esc in zen mode.
    pub(super) fn exit_zen(&mut self, _: &ExitZen, window: &mut Window, cx: &mut Context<Self>) {
        // A terminal's program may want its Esc.
        if self.active.and_then(|a| self.tabs.get(a)).is_some_and(|t| matches!(t.kind, TabKind::Terminal(_))) {
            return cx.propagate();
        }
        self.set_zen(false, window, cx);
    }

    pub(super) fn render_zen_note(&self) -> Option<AnyElement> {
        self.shell.zen_note.as_ref()?;
        Some(
            div()
                .absolute()
                .bottom(px(16.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("zen-note")
                        .debug_selector(|| "zen-note".into())
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .shadow_lg()
                        .text_size(px(12.))
                        .text_color(theme::fg_bright())
                        .child("Zen mode. ⌘K Z or Esc brings everything back."),
                )
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------ hints after ⌘K

    /// The keys typed so far changed: after ⌘K, the hints come up once it has waited a moment;
    /// when the chord is done or cancelled they go.
    pub(super) fn chord_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let after_cmd_k = window.pending_input_keystrokes().is_some_and(|keys| {
            matches!(keys, [k] if k.key == "k" && k.modifiers.platform && !k.modifiers.control && !k.modifiers.alt && !k.modifiers.shift)
        });
        if !after_cmd_k {
            self.shell.chord_task = None;
            if std::mem::take(&mut self.shell.chord_ready) {
                cx.notify();
            }
            return;
        }
        if self.shell.chord_ready || self.shell.chord_task.is_some() {
            return;
        }
        self.shell.chord_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(CHORD_DELAY_MS)).await;
            this.update_in(cx, |ws, window, cx| {
                ws.shell.chord_task = None;
                ws.shell.chord_ready = window.has_pending_keystrokes();
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn render_chord_hints(&self, window: &Window) -> Option<AnyElement> {
        if !self.shell.chord_ready {
            return None;
        }
        let typed = window.pending_input_keystrokes()?;
        let head = typed.iter().map(|k| crate::keybindings::label(&k.unparse(gpui::OperatingSystem::Mac))).collect::<Vec<_>>().join(" ");
        let rows = crate::keybindings::follow_ups(window);
        let row = |key: String, what: String| {
            div()
                .id(SharedString::from(format!("chord-{key}")))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().w(px(48.)).flex().child(kbd(key)))
                .child(div().text_color(theme::fg()).whitespace_nowrap().child(what))
        };
        Some(
            div()
                .id("chord-hints")
                .debug_selector(|| "chord-hints".into())
                .absolute()
                .bottom(px(if self.shell.zen { 12. } else { STATUS_H + 8. }))
                .left(px(if self.shell.zen { 12. } else { ACTIVITY_W + 12. }))
                .flex()
                .flex_col()
                .gap(px(4.))
                .p(px(10.))
                .bg(theme::bg_popup())
                .border_1()
                .border_color(theme::border_input())
                .rounded(px(8.))
                .shadow_lg()
                .text_size(px(12.))
                .child(div().flex().gap(px(6.)).items_center().text_color(theme::fg_bright()).child(kbd(head)).child("then"))
                .children(rows.into_iter().map(|(k, d)| row(k, d)))
                .child(row("Esc".into(), "Cancel".into()))
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------ title bar

    /// Recent projects other than this one, as the switcher and the start screen list them.
    fn recent_projects(&self, cx: &App) -> Vec<PathBuf> {
        let current = self.root(cx);
        let is_temp = |f: &Path| f.starts_with(std::env::temp_dir()) || f.starts_with("/private/tmp") || f.starts_with("/tmp") || f.starts_with("/private/var/folders");
        self.settings.recent_folders.iter().filter(|f| f.is_dir() && !is_temp(f) && **f != current).cloned().collect()
    }

    pub(super) fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.shell.zen {
            return None;
        }
        let compact = self.shell.stack;
        let name = if self.has_project { self.root(cx).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() } else { "No folder".into() };
        let button = |id: &'static str, icon: &'static str, tip: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(5.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .tooltip(ui::tooltip(tip))
                .child(ui::small_icon(icon, if on { theme::accent() } else { theme::fg_muted() }))
        };
        let find_label = shortcut("FindFile", "⌘P");
        Some(
            div()
                .id("title-bar")
                .debug_selector(|| "title-bar".into())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(TITLE_H))
                // The window buttons sit at the left, over the bar.
                .pl(if window.is_fullscreen() { px(8.) } else { gpui::px(LIGHTS_W) })
                .pr(px(8.))
                .bg(self.chrome_bg())
                .border_b_1()
                .border_color(self.chrome_border())
                .text_size(px(12.))
                .child(button("title-sidebar", "sidebar", "Show or hide the sidebar (⌘B)", self.side_visible).on_click(cx.listener(|ws, _, w, cx| ws.toggle_sidebar(&ToggleSidebar, w, cx))))
                .child(
                    div()
                        .id("title-project")
                        .debug_selector(|| "title-project".into())
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(24.))
                        .px(px(8.))
                        .min_w_0()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::fg_bright())
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tooltip("Projects and windows"))
                        // The click that opens the menu is not an outside click.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(ui::small_icon("folder", theme::fg_muted()))
                        .child(div().max_w(px(if compact { 90. } else { 220. })).overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                        .child(ui::small_icon("chevron-down", theme::fg_muted()).size(px(10.)))
                        .on_click(cx.listener(|ws, _, w, cx| {
                            ws.shell.project_menu = !ws.shell.project_menu;
                            if !ws.shell.project_menu && ws.cancel_color_input(cx) {
                                ws.focus_active(w, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div().flex().flex_1().min_w_0().justify_center().child(
                        div()
                            .id("title-cmd")
                            .debug_selector(|| "title-cmd".into())
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .w_full()
                            .max_w(px(440.))
                            .min_w_0()
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(px(7.))
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::bg_editor())
                            .text_color(theme::fg_muted())
                            .cursor_pointer()
                            .hover(|s| s.border_color(theme::border_input()).text_color(theme::fg_dim()))
                            .child(ui::small_icon("search", theme::fg_muted()).size(px(12.)))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child("Go to file or command"))
                            .when(!compact, |d| d.child(kbd(find_label)))
                            .on_click(cx.listener(|ws, _, w, cx| ws.open_cmd(CmdMode::Files, "", w, cx))),
                    ),
                )
                .when(!compact, |d| d.child(button("title-split", "split", "Split right (⌘\\)", false).on_click(cx.listener(|ws, _, w, cx| ws.split(true, w, cx)))))
                .child(button("title-panel", "panel", "Show or hide the output panel (⌘J)", self.output_visible).on_click(cx.listener(|ws, _, w, cx| ws.toggle_output(&ToggleOutput, w, cx))))
                .into_any_element(),
        )
    }

    /// The project switcher: this project, the recent ones, and the ways to open another.
    pub(super) fn render_project_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.shell.project_menu || self.shell.zen {
            return None;
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let heading = |text: &str| div().px(px(10.)).pt(px(6.)).pb(px(2.)).text_size(px(11.)).text_color(theme::fg_muted()).child(text.to_string());
        let row = |id: SharedString, icon: &'static str, label: String, detail: String| {
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(26.))
                .px(px(10.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_color(theme::fg())
                .hover(|s| s.bg(theme::hover()))
                .child(ui::small_icon(icon, theme::fg_muted()))
                .child(div().flex_none().max_w(px(150.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_bright()).child(label))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.)).text_color(theme::fg_muted()).child(detail))
        };
        let name = self.root(cx).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let recent: Vec<PathBuf> = self.recent_projects(cx).into_iter().take(8).collect();
        let (open, new_window) = (shortcut("OpenFolder", "⌘O"), shortcut("OpenFolderNewWindow", "⌘⇧N"));
        let tail = |label: &str, keys: String| (label.to_string(), keys);
        let (open_row, new_row) = (tail("Open folder…", open), tail("New window…", new_window));
        Some(
            div()
                .id("project-menu")
                .debug_selector(|| "project-menu".into())
                .occlude()
                .absolute()
                .top(px(TITLE_H - 4.))
                .left(if window.is_fullscreen() { px(8. + 30.) } else { gpui::px(LIGHTS_W) + px(30.) })
                .w(px(300.))
                .max_w(relative(0.96))
                .flex()
                .flex_col()
                .p(px(4.))
                .bg(theme::bg_popup())
                .border_1()
                .border_color(theme::border_input())
                .rounded(px(8.))
                .shadow_lg()
                .text_size(px(12.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(self.has_project, |d| d.child(heading("This window")).child(row("project-current".into(), "folder", name, "open now".into())).child(self.render_color_row(cx)))
                .when(!recent.is_empty(), |d| d.child(heading("Recent projects")))
                .children(recent.into_iter().enumerate().map(|(i, dir)| {
                    let label = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let parent = dir.parent().map(|p| p.to_string_lossy().replacen(&home, "~", 1)).unwrap_or_default();
                    row(SharedString::from(format!("project-recent-{i}")), "folder", label, parent).on_click(cx.listener(move |ws, _, w, cx| {
                        ws.shell.project_menu = false;
                        ws.switch_project(dir.clone(), w, cx);
                    }))
                }))
                .child(div().mx(px(6.)).my(px(4.)).h(px(1.)).bg(theme::border()))
                .child(row("project-open".into(), "open", open_row.0, open_row.1).on_click(cx.listener(|ws, _, w, cx| {
                    ws.shell.project_menu = false;
                    ws.open_folder(&OpenFolder, w, cx);
                })))
                .child(row("project-new-window".into(), "plus", new_row.0, new_row.1).on_click(cx.listener(|ws, _, w, cx| {
                    ws.shell.project_menu = false;
                    ws.pick_folder(true, w, cx);
                })))
                .into_any_element(),
        )
    }

    /// The title bar's and status bar's background: the window color mixed in, if there is one.
    pub(super) fn chrome_bg(&self) -> Hsla {
        match self.shell.color {
            Some(c) => theme::bg_side().blend(Hsla::from(rgb(c)).opacity(TINT)),
            None => theme::bg_side(),
        }
    }

    pub(super) fn chrome_border(&self) -> Hsla {
        self.shell.color.map_or(theme::border(), |c| Hsla::from(rgb(c)).opacity(0.6))
    }

    /// Sets this window's title bar color (None: the theme's) and saves it with the project.
    pub(super) fn set_window_color(&mut self, color: Option<u32>, cx: &mut Context<Self>) {
        self.shell.color = color;
        self.save_session(cx);
        cx.notify();
    }

    /// Custom…: a hex field under the swatches; the bars follow what's typed while it is valid.
    fn start_color_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.shell.color;
        let input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.font_size = 12.;
            ed.placeholder = "#rrggbb".into();
            ed.set_text(&before.map(|c| format!("#{c:06x}")).unwrap_or_else(|| "#".into()), cx);
            let len = ed.buf.len();
            ed.select_range(len..len, cx);
            ed
        });
        let sub = cx.subscribe_in(&input, window, |ws, input, ev: &EditorEvent, window, cx| match ev {
            EditorEvent::Changed => {
                if let Some(c) = parse_window_color(&input.read(cx).text()) {
                    ws.shell.color = Some(c);
                    cx.notify();
                }
            }
            EditorEvent::Submit => {
                let text = input.read(cx).text();
                match parse_window_color(&text) {
                    Some(c) => {
                        ws.shell.color_input = None;
                        ws.shell.project_menu = false;
                        ws.set_window_color(Some(c), cx);
                        ws.focus_active(window, cx);
                    }
                    None => ws.flash(format!("Not a color: {}. Use #rrggbb", text.trim()), true, cx),
                }
            }
            EditorEvent::Cancel => {
                ws.shell.project_menu = false;
                ws.cancel_color_input(cx);
                ws.focus_active(window, cx);
            }
            _ => {}
        });
        window.focus(&input.read(cx).focus_target(cx), cx);
        self.shell.color_input = Some((input, before, sub));
        cx.notify();
    }

    /// Leaves Custom… and puts back the color from before it.
    /// True when it was open (then the field that had focus is gone).
    pub(super) fn cancel_color_input(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((_, before, _)) = self.shell.color_input.take() else { return false };
        self.shell.color = before;
        cx.notify();
        true
    }

    /// `:window-color <#rrggbb | name | off>`.
    pub(super) fn set_window_color_from(&mut self, text: &str, cx: &mut Context<Self>) {
        match text.trim() {
            "off" | "none" | "" => self.set_window_color(None, cx),
            t => match parse_window_color(t) {
                Some(c) => self.set_window_color(Some(c), cx),
                None => self.flash(format!("Not a color: {t}. Use #rrggbb or one of: {}", WINDOW_COLORS.map(|(n, _)| n).join(", ")), true, cx),
            },
        }
    }

    /// The window color: none, the presets, and any other through the command line.
    fn render_color_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.shell.color;
        let swatch = |id: SharedString, fill: Option<u32>, tip: String| {
            let on = fill == current;
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(18.))
                .rounded_full()
                .cursor_pointer()
                .border_2()
                .border_color(if on { theme::fg_bright() } else { gpui::transparent_black() })
                .tooltip(ui::tooltip_text(tip))
                .child(match fill {
                    Some(c) => div().size(px(12.)).rounded_full().bg(rgb(c)).into_any_element(),
                    None => div().size(px(12.)).rounded_full().border_1().border_color(theme::fg_muted()).into_any_element(),
                })
                .on_click(cx.listener(move |ws, _, _, cx| ws.set_window_color(fill, cx)))
        };
        let custom = current.filter(|c| !WINDOW_COLORS.iter().any(|(_, p)| p == c));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .px(px(10.))
            .py(px(4.))
            .child(div().flex_none().mr(px(4.)).text_color(theme::fg_muted()).text_size(px(11.)).child("Color"))
            .child(swatch("window-color-none".into(), None, "No color".into()))
            .children(WINDOW_COLORS.iter().map(|(n, c)| swatch(format!("window-color-{n}").into(), Some(*c), format!("{}{}", n[..1].to_uppercase(), &n[1..]))))
            .children(custom.map(|c| swatch("window-color-custom".into(), Some(c), format!("#{c:06x}"))))
            .child(
                div()
                    .id("window-color-other")
                    .debug_selector(|| "window-color-other".into())
                    .flex_none()
                    .px(px(6.))
                    .h(px(18.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(theme::fg_dim())
                    .hover(|s| s.bg(theme::hover()))
                    .tooltip(ui::tooltip("Any color: type #rrggbb"))
                    .child("Custom…")
                    .on_click(cx.listener(|ws, _, w, cx| ws.start_color_input(w, cx))),
            )
            .children(self.shell.color_input.as_ref().map(|(input, _, _)| {
                div()
                    .id("window-color-input")
                    .debug_selector(|| "window-color-input".into())
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .mt(px(2.))
                    .child(div().flex_1().min_w_0().h(px(24.)).px(px(6.)).flex().items_center().rounded(px(4.)).border_1().border_color(theme::focus_border()).bg(theme::bg_input()).child(input.clone()))
                    .child(div().flex_none().text_size(px(11.)).text_color(theme::fg_muted()).child("↵ keep · esc cancel"))
            }))
            .into_any_element()
    }

    // ------------------------------------------------------------------ breadcrumb

    /// Under a file's tabs: where it is, and what can be done with it.
    pub(super) fn render_breadcrumb(&self, id: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ix = self.pane(id)?.active.and_then(|t| self.tab_ix(t))?;
        let TabKind::Editor(e) = &self.tabs[ix].kind else { return None };
        let (path, blame, symbol) = {
            let e = e.read(cx);
            // The function or type the cursor is in (and what holds it).
            let symbols = e.symbols();
            let names: Vec<String> = crate::symbols::around(&symbols, e.cursor_rc().0).iter().rev().take(2).rev().map(|s| s.name.clone()).collect();
            (e.path.clone()?, e.blame_gutter, names.join(" › "))
        };
        let home = std::env::var("HOME").unwrap_or_default();
        let shown = self.rel(&path, cx).replacen(&home, "~", 1);
        let segs: Vec<String> = shown.split('/').filter(|s| !s.is_empty()).map(String::from).collect();
        // The part that is changed against the index, in the repository the file belongs to.
        let repo = self.repo_for(&path, cx);
        let (root, rel) = {
            let r = repo.read(cx);
            (r.root.clone(), path.strip_prefix(&r.root).ok().map(|p| p.to_string_lossy().to_string()))
        };
        let changed = rel.as_ref().is_some_and(|rel| repo.read(cx).status.unstaged.iter().any(|c| c.path == *rel));
        let can_preview = preview::has_preview(&path);
        let finding = self.find_on.as_ref().is_some_and(|(p, _)| *p == id);

        // shortcut: the room for the path is estimated by character count, not measured; upgrade if the cut shows.
        let pane_w = self.pane_bounds.borrow().get(&id).map(|b| b.size.width / gpui::px(1.) / ui::scale()).unwrap_or(f32::MAX);
        let actions_w = 4. * 26. + if changed { 80. } else { 0. } + if can_preview { 26. } else { 0. } + if symbol.is_empty() { 0. } else { 24. + symbol.chars().count().min(32) as f32 * 7. };
        let fits = ((pane_w - actions_w - 30.) / 6.8).max(8.);
        let cost = |from: usize| segs[from..].iter().map(|s| s.chars().count() + 3).sum::<usize>() as f32;
        let mut from = 0;
        while from + 1 < segs.len() && cost(from) > fits {
            from += 1;
        }
        let last = segs.len().saturating_sub(1);

        let action = |name: &'static str, icon: &'static str, tip: &'static str, on: bool| {
            let key = SharedString::from(format!("{name}-{id}"));
            div()
                .id(key.clone())
                .debug_selector(move || key.to_string())
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(22.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .tooltip(ui::tooltip(tip))
                .child(ui::small_icon(icon, if on { theme::accent() } else { theme::fg_muted() }))
        };
        let history_path = path.clone();
        Some(
            div()
                .id(SharedString::from(format!("crumb-{id}")))
                .debug_selector(move || format!("crumb-{id}"))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .h(px(24.))
                .pl(px(14.))
                .pr(px(6.))
                .overflow_hidden()
                .border_b_1()
                .border_color(theme::border())
                .text_size(px(12.))
                .text_color(theme::fg_muted())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .when(from > 0, |d| d.child("…").child(div().text_color(theme::fg_muted()).child("›")))
                        .children(segs.iter().enumerate().skip(from).flat_map(|(i, s)| {
                            let seg = div().child(s.clone()).when(i == last, |d| d.text_color(theme::fg_bright()));
                            let sep = (i != last).then(|| div().text_color(theme::fg_muted()).child("›"));
                            [Some(seg), sep]
                        }).flatten()),
                )
                // The current symbol: click to list the file's symbols.
                .child(
                    div()
                        .id(SharedString::from(format!("crumb-symbol-{id}")))
                        .debug_selector(move || format!("crumb-symbol-{id}"))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(4.))
                        .max_w(px(260.))
                        .overflow_hidden()
                        .when(!symbol.is_empty(), |d| {
                            d.h(px(22.))
                                .px(px(5.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::hover()))
                                .tooltip(ui::tooltip("Go to a symbol in this file (⌘⇧O)"))
                                .child(div().text_color(theme::fg_muted()).child("›"))
                                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::fg_bright()).child(symbol.clone()))
                                .on_click(cx.listener(move |ws, _, w, cx| {
                                    ws.set_focused_pane(id);
                                    ws.go_to_symbol(&GoToSymbol, w, cx);
                                }))
                        }),
                )
                .child(div().flex_1())
                .when(changed, |d| {
                    let (root, rel) = (root.clone(), rel.clone().unwrap_or_default());
                    d.child(
                        div()
                            .id(SharedString::from(format!("crumb-changes-{id}")))
                            .debug_selector(move || format!("crumb-changes-{id}"))
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(5.))
                            .h(px(22.))
                            .px(px(7.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .tooltip(ui::tooltip("Show what changed in this file against the index"))
                            .child(div().size(px(6.)).rounded_full().bg(theme::git_modified()))
                            .child("Changes")
                            .on_click(cx.listener(move |ws, _, w, cx| {
                                ws.set_focused_pane(id);
                                if ws.git_root(cx) != root {
                                    ws.select_repo(&root, w, cx);
                                }
                                ws.open_diff(DiffSpec { path: rel.clone(), old_path: None, left: Rev::Index, right: Rev::Working, back: None }, false, w, cx);
                            })),
                    )
                })
                .when(can_preview, |d| {
                    d.child(action("crumb-preview", "eye", "Preview in place (⇧⌘V); ⌘K V opens it beside", false).on_click(cx.listener(move |ws, _, w, cx| {
                        ws.set_focused_pane(id);
                        ws.toggle_preview(&TogglePreview, w, cx);
                    })))
                })
                .child(action("crumb-history", "history", "This file's history", false).on_click(cx.listener(move |ws, _, w, cx| {
                    ws.handle_request(&Request::FileHistory(history_path.clone()), w, cx);
                })))
                .child(action("crumb-blame", "blame", "Blame: who wrote each line (⌥⌘B)", blame).on_click(cx.listener(move |ws, _, w, cx| {
                    ws.set_focused_pane(id);
                    ws.toggle_blame(&ToggleBlame, w, cx);
                })))
                .child(action("crumb-find", "search", "Find in this file (⌘F)", finding).on_click(cx.listener(move |ws, _, w, cx| {
                    ws.set_focused_pane(id);
                    ws.open_find(false, w, cx);
                })))
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------ start screen

    fn start_row(&self, key: &'static str, pane: usize, label: &str, keys: &str, cx: &mut Context<Self>, act: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static) -> Stateful<Div> {
        let sel = format!("start-{key}-{pane}");
        div()
            .id(SharedString::from(sel.clone()))
            .debug_selector(move || sel.clone())
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(26.))
            .px(px(8.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_color(theme::fg())
            .hover(|s| s.bg(theme::hover()))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label.to_string()))
            .children(keys.split(" or ").filter(|k| !k.is_empty()).map(|k| kbd(k.to_string())))
            .on_click(cx.listener(move |ws, _, w, cx| {
                ws.set_focused_pane(pane);
                act(ws, w, cx);
            }))
    }

    /// An empty pane or split: what the project is, and what to do next.
    pub(super) fn render_start(&self, pane: usize, cx: &mut Context<Self>) -> AnyElement {
        let name = self.root(cx).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let branch = self.git.read(cx).branch_label();
        let nothing = if self.panes.len() > 1 { "Nothing is open in this split yet." } else { "Nothing is open yet." };
        let heading = |text: &str| div().mb(px(6.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_muted()).child(text.to_string());
        let command = format!("{} or :", shortcut("CommandPalette", "⌘;"));

        let start = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(220.))
            .child(heading("Start here"))
            .child(self.start_row("file", pane, "Go to file", &shortcut("FindFile", "⌘P"), cx, |ws, w, cx| ws.open_cmd(CmdMode::Files, "", w, cx)))
            .child(self.start_row("command", pane, "Command line", &command, cx, |ws, w, cx| ws.open_cmd(CmdMode::Command, "", w, cx)))
            .child(self.start_row("search", pane, "Search in files", &shortcut("SearchAll", "⇧⌘F"), cx, |ws, w, cx| ws.search_all(&SearchAll, w, cx)))
            .child(self.start_row("git", pane, "Source control", &shortcut("ShowGit", "⌃⇧G"), cx, |ws, w, cx| ws.show_git(&ShowGit, w, cx)))
            .child(self.start_row("terminal", pane, "Terminal in this split", &shortcut("NewTerminal", "⌘T"), cx, |ws, w, cx| ws.open_terminal_view(None, w, cx)))
            .child(self.start_row("split", pane, "Split right", &shortcut("SplitRight", "⌘\\"), cx, |ws, w, cx| ws.split(true, w, cx)))
            .child(self.start_row("claude", pane, "Ask Claude", &shortcut("OpenClaude", "⌘Esc"), cx, |ws, w, cx| ws.open_agent(false, None, None, w, cx)))
            .child(self.start_row("keys", pane, "Every shortcut", &shortcut("ShowKeysPanel", "⌘K ⌘S"), cx, |ws, w, cx| ws.show_keys_panel(&ShowKeysPanel, w, cx)));

        // Recent projects are on the screen shown before a project is open, not here.
        let sel = format!("start-screen-{pane}");
        div()
            .id(SharedString::from(sel.clone()))
            .debug_selector(move || sel.clone())
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(theme::bg_editor())
            .text_size(px(13.))
            .text_color(theme::fg_dim())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_x(px(40.))
                    .gap_y(px(24.))
                    .w_full()
                    .max_w(px(360.))
                    .p(px(24.))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child("OSKL Code"))
                            .child(div().child(if branch.is_empty() { format!("{name}. {nothing}") } else { format!("{name}, on {branch}. {nothing}") })),
                    )
                    .child(start),
            )
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;

#[cfg(test)]
mod color_tests {
    use super::parse_window_color;

    #[test]
    fn window_colors_parse() {
        assert_eq!(parse_window_color("#3F8F8A"), Some(0x3f8f8a));
        assert_eq!(parse_window_color("3f8f8a"), Some(0x3f8f8a));
        assert_eq!(parse_window_color("#38a"), Some(0x3388aa));
        assert_eq!(parse_window_color("Teal"), Some(0x3f8f8a));
        assert_eq!(parse_window_color("#12345"), None);
        assert_eq!(parse_window_color("red-ish"), None);
    }
}
