//! Colors and sizes. The default look is modelled on VS Code's "Dark Modern"; Settings can
//! switch to the other themes here, live.

use crate::ui::px;
use gpui::{Hsla, Pixels, rgb, rgba};

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn ca(hex_rgba: u32) -> Hsla {
    rgba(hex_rgba).into()
}

pub const UI_FONT: &str = "Menlo";
pub const MONO_FONT: &str = "Menlo";

/// The color themes. Default is the original look; the others are picked in Settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Default,
    Dark,
    Light,
    Blue,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::Default, Theme::Dark, Theme::Light, Theme::Blue];

    /// Its name in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Theme::Default => "default",
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Blue => "blue",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::Default => "Default",
            Theme::Dark => "Dark Modern",
            Theme::Light => "Light",
            Theme::Blue => "Blue Night",
        }
    }

    pub fn from_id(id: &str) -> Theme {
        Theme::ALL.into_iter().find(|t| t.id() == id).unwrap_or(Theme::Default)
    }
}

/// App-wide; in tests, per thread (like the interface zoom).
#[cfg(not(test))]
static THEME: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
thread_local!(static THEME: std::sync::atomic::AtomicU8 = const { std::sync::atomic::AtomicU8::new(0) });

fn theme_ix() -> usize {
    #[cfg(not(test))]
    return THEME.load(std::sync::atomic::Ordering::Relaxed) as usize;
    #[cfg(test)]
    return THEME.with(|t| t.load(std::sync::atomic::Ordering::Relaxed)) as usize;
}

pub fn current() -> Theme {
    Theme::ALL[theme_ix() % Theme::ALL.len()]
}

pub fn set(t: Theme) {
    let ix = Theme::ALL.iter().position(|x| *x == t).unwrap_or(0) as u8;
    #[cfg(not(test))]
    THEME.store(ix, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    THEME.with(|x| x.store(ix, std::sync::atomic::Ordering::Relaxed));
}

/// Three colors that show what a theme looks like: background, sidebar, accent.
pub fn preview(t: Theme) -> [u32; 3] {
    match t {
        Theme::Default => [0x111318, 0x0c0e11, 0x98c379],
        Theme::Dark => [0x1f1f1f, 0x181818, 0x0078d4],
        Theme::Light => [0xffffff, 0xf0f0f0, 0x005fb8],
        Theme::Blue => [0x0f1b2d, 0x0b1524, 0x5ccfe6],
    }
}

/// One value per theme: (default, dark, light, blue).
fn pick<T>(default: T, dark: T, light: T, blue: T) -> T {
    match current() {
        Theme::Default => default,
        Theme::Dark => dark,
        Theme::Light => light,
        Theme::Blue => blue,
    }
}

macro_rules! themed {
    ($($(#[$m:meta])* $name:ident: $d:expr, $k:expr, $l:expr, $b:expr;)*) => {
        $($(#[$m])* pub fn $name() -> Hsla { c(pick($d, $k, $l, $b)) })*
    };
}
macro_rules! themed_a {
    ($($(#[$m:meta])* $name:ident: $d:expr, $k:expr, $l:expr, $b:expr;)*) => {
        $($(#[$m])* pub fn $name() -> Hsla { ca(pick($d, $k, $l, $b)) })*
    };
}

pub fn term_bg() -> u32 { pick(0x111318, 0x1f1f1f, 0xffffff, 0x0f1b2d) }
pub fn term_fg() -> u32 { pick(0xc8ccd4, 0xcccccc, 0x383a42, 0xc6d4e8) }

/// The terminal's 16 colors.
pub fn ansi(ix: usize) -> u32 {
    const DARK: [u32; 16] = [
        0x1d1f21, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd7dae0,
        0x5c6370, 0xff7a85, 0xb5e890, 0xffd68a, 0x82c4ff, 0xde9bf0, 0x7fd4dd, 0xffffff,
    ];
    const LIGHT: [u32; 16] = [
        0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0xa0a1a7,
        0x696c77, 0xca1243, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0x383a42,
    ];
    pick(DARK, DARK, LIGHT, DARK)[ix % 16]
}

themed! {
    /// Frame color of the focused pane (zellij-style).
    frame_focused: 0x98c379, 0x0078d4, 0x005fb8, 0x5ccfe6;
    frame: 0x2f343e, 0x2b2b2b, 0xd8d8d8, 0x1e3150;
    bg_app: 0x0c0e11, 0x181818, 0xf0f0f0, 0x0b1524;
}

pub fn ui_text() -> Pixels {
    px(11.5)
}

pub fn row_h() -> Pixels {
    px(20.)
}

themed! {
    // Surfaces
    bg_editor: 0x111318, 0x1f1f1f, 0xffffff, 0x0f1b2d;
    bg_side: 0x0c0e11, 0x181818, 0xf8f8f8, 0x0b1524;
    bg_input: 0x171a20, 0x313131, 0xffffff, 0x13233a;
    bg_popup: 0x15181e, 0x202020, 0xffffff, 0x122036;
    border: 0x262a33, 0x2b2b2b, 0xe5e5e5, 0x1e3150;
    border_input: 0x343a46, 0x3c3c3c, 0xcecece, 0x2a4266;
    focus_border: 0x98c379, 0x0078d4, 0x005fb8, 0x5ccfe6;

    // Text
    fg: 0xc8ccd4, 0xcccccc, 0x3b3b3b, 0xc6d4e8;
    fg_bright: 0xeceff4, 0xffffff, 0x1f1f1f, 0xeef4fc;
    fg_dim: 0x8b919c, 0x9d9d9d, 0x616161, 0x8aa0bf;
    fg_muted: 0x5c6370, 0x6e7681, 0x8b8b8b, 0x5b7090;

    // Accents
    accent: 0x61afef, 0x0078d4, 0x005fb8, 0x4fa8ff;
    accent_hover: 0x7cbcf2, 0x026ec1, 0x0258a8, 0x6cb8ff;
    hover: 0x1b1f27, 0x2a2d2e, 0xebebeb, 0x15253d;
    selected_focused: 0x253447, 0x04395e, 0xd6ebff, 0x1d3a63;
    selected: 0x20242c, 0x37373d, 0xe4e6f1, 0x172a45;
    badge: 0x3a404c, 0x616161, 0xcccccc, 0x2a4266;

    // Editor
    selection: 0x2d4b6e, 0x264f78, 0xadd6ff, 0x1f4a7a;
    selection_inactive: 0x2a2f38, 0x3a3d41, 0xe5ebf1, 0x1a3150;
    cursor: 0xaeafad, 0xaeafad, 0x000000, 0x80cbff;
    line_number: 0x4b515c, 0x6e7681, 0x9e9e9e, 0x3e5577;
    indent_guide: 0x262a32, 0x404040, 0xe0e0e0, 0x1a2c47;
    bracket_match_border: 0x6b7280, 0x888888, 0xb9b9b9, 0x4d6a92;
    line_number_active: 0xcccccc, 0xcccccc, 0x171184, 0xc6d4e8;

    // Git
    git_modified: 0xe2c08d, 0xe2c08d, 0x895503, 0xe2c08d;
    git_added: 0x73c991, 0x73c991, 0x587c0c, 0x73c991;
    git_deleted: 0xc74e39, 0xc74e39, 0xad0707, 0xf07178;
    git_conflict: 0xe4676b, 0xe4676b, 0xad0707, 0xe4676b;
    git_ignored: 0x8c8c8c, 0x8c8c8c, 0x8e8e90, 0x6a7f9e;
    gutter_added: 0x2ea043, 0x2ea043, 0x2ea043, 0x2ea043;
    gutter_modified: 0x0078d4, 0x0078d4, 0x0078d4, 0x4fa8ff;
    gutter_deleted: 0xf85149, 0xf85149, 0xf85149, 0xf85149;
}

themed_a! {
    line_highlight: 0xffffff0a, 0xffffff0a, 0x0000000a, 0xffffff0a;
    bracket_match: 0x5c637033, 0x5c637033, 0x0064001a, 0x5c637033;
    find_match: 0xea5c0055, 0xea5c0055, 0xf8c84466, 0xea5c0055;
    scrollbar: 0x79797966, 0x79797966, 0x64646466, 0x4d6a9266;

    // Diff
    diff_ins_line: 0x2ea04326, 0x2ea04326, 0x2ea04322, 0x2ea04326;
    diff_del_line: 0xf8514926, 0xf8514926, 0xf8514922, 0xf8514926;
    diff_ins_char: 0x2ea04366, 0x2ea04366, 0x2ea04355, 0x2ea04366;
    diff_del_char: 0xf8514966, 0xf8514966, 0xf8514955, 0xf8514966;
    diff_filler: 0xffffff06, 0xffffff06, 0x00000008, 0xffffff06;
}

/// What a syntax-highlighting capture is, for picking its color.
#[derive(Clone, Copy)]
pub enum Syntax {
    Comment, Builtin, Param, FnBuiltin, PunctSpecial, StringSpecial, Keyword, String, Number,
    Function, Type, Property, Attribute, Operator, Punct, Plain,
}

pub fn syntax(s: Syntax) -> Hsla {
    use Syntax::*;
    // (default, dark, light, blue), each in the order of `Syntax`.
    let row: [u32; 16] = pick(
        [0x5c6370, 0xe5c07b, 0xd19a66, 0x56b6c2, 0xc678dd, 0x56b6c2, 0xc678dd, 0x98c379, 0xd19a66, 0x61afef, 0xe5c07b, 0xe06c75, 0xd19a66, 0x56b6c2, 0x8b919c, 0xc8ccd4],
        [0x6a9955, 0x4fc1ff, 0x9cdcfe, 0xdcdcaa, 0xc586c0, 0xd7ba7d, 0xc586c0, 0xce9178, 0xb5cea8, 0xdcdcaa, 0x4ec9b0, 0x9cdcfe, 0x9cdcfe, 0xd4d4d4, 0xd4d4d4, 0xd4d4d4],
        [0xa0a1a7, 0xc18401, 0x986801, 0x0184bc, 0xa626a4, 0x0184bc, 0xa626a4, 0x50a14f, 0x986801, 0x4078f2, 0xc18401, 0xe45649, 0x986801, 0x0184bc, 0x383a42, 0x383a42],
        [0x5f7a9e, 0x2ac3de, 0xe0af68, 0x2ac3de, 0xbb9af7, 0x89ddff, 0xbb9af7, 0x9ece6a, 0xff9e64, 0x7aa2f7, 0x2ac3de, 0x73daca, 0xe0af68, 0x89ddff, 0x9aa5ce, 0xc0caf5],
    );
    let ix = match s {
        Comment => 0, Builtin => 1, Param => 2, FnBuiltin => 3, PunctSpecial => 4, StringSpecial => 5, Keyword => 6, String => 7,
        Number => 8, Function => 9, Type => 10, Property => 11, Attribute => 12, Operator => 13, Punct => 14, Plain => 15,
    };
    c(row[ix])
}

// Graph
pub fn ref_local() -> Hsla { c(0x3794ff) }
pub fn ref_remote() -> Hsla { c(0xe5613d) }
pub fn ref_tag() -> Hsla { c(0x8c8c8c) }

pub fn lane_colors() -> [Hsla; 6] {
    [
        c(0x4daafc), // current branch
        c(0xffb000),
        c(0xdc267f),
        c(0xb66dff),
        c(0x40b0a6),
        c(0x994f00),
    ]
}

/// Heat color for blame: newer commits are brighter.
pub fn blame_age(age_days: f64) -> Hsla {
    let t = (1.0 - (age_days.max(0.0) / 365.0).min(1.0)) as f32;
    Hsla { h: 0.08, s: 0.75, l: 0.25 + 0.35 * t, a: 0.35 + 0.65 * t }
}
