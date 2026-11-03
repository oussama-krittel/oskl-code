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
    SoftGray,
    /// A darker gray than Soft Gray, still with dark text.
    Stone,
    Arctic,
    Ember,
    Grape,
}

impl Theme {
    pub const ALL: [Theme; 9] = [Theme::Default, Theme::Dark, Theme::Light, Theme::Blue, Theme::SoftGray, Theme::Stone, Theme::Arctic, Theme::Ember, Theme::Grape];

    /// Its name in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Theme::Default => "default",
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Blue => "blue",
            Theme::SoftGray => "softgray",
            Theme::Stone => "stone",
            Theme::Arctic => "arctic",
            Theme::Ember => "ember",
            Theme::Grape => "grape",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::Default => "Default",
            Theme::Dark => "Dark Modern",
            Theme::Light => "Light",
            Theme::Blue => "Blue Night",
            Theme::SoftGray => "Soft Gray",
            Theme::Stone => "Stone Gray",
            Theme::Arctic => "Arctic",
            Theme::Ember => "Ember",
            Theme::Grape => "Grape",
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

/// Text that reads on `bg`: near-black on a bright fill, white on a dark one (whatever the theme).
pub fn on(bg: Hsla) -> Hsla {
    let c = bg.to_rgb();
    let luminance = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    if luminance > 0.5 { self::c(0x15171c) } else { self::c(0xffffff) }
}

/// Three colors that show what a theme looks like: background, sidebar, accent.
pub fn preview(t: Theme) -> [u32; 3] {
    match t {
        Theme::Default => [0x111318, 0x0c0e11, 0x98c379],
        Theme::Dark => [0x1f1f1f, 0x181818, 0x0078d4],
        Theme::Light => [0xffffff, 0xf0f0f0, 0x005fb8],
        Theme::Blue => [0x0f1b2d, 0x0b1524, 0x5ccfe6],
        Theme::SoftGray => [0xdcdde0, 0xc9cacf, 0x3f67a8],
        Theme::Stone => [0xc8cacf, 0xb4b6bc, 0x345a96],
        Theme::Arctic => [0x2e3440, 0x242933, 0x88c0d0],
        Theme::Ember => [0x282828, 0x1d2021, 0xd79921],
        Theme::Grape => [0x282a36, 0x1e1f29, 0xbd93f9],
    }
}

/// One value per theme, in the order of `Theme::ALL`.
fn pick<T: Copy>(values: [T; 9]) -> T {
    values[theme_ix() % values.len()]
}

macro_rules! themed {
    ($($(#[$m:meta])* $name:ident: $($v:expr),+;)*) => {
        $($(#[$m])* pub fn $name() -> Hsla { c(pick([$($v),+])) })*
    };
}
macro_rules! themed_a {
    ($($(#[$m:meta])* $name:ident: $($v:expr),+;)*) => {
        $($(#[$m])* pub fn $name() -> Hsla { ca(pick([$($v),+])) })*
    };
}

pub fn term_bg() -> u32 { pick([0x111318, 0x1f1f1f, 0xffffff, 0x0f1b2d, 0xdcdde0, 0xc8cacf, 0x2e3440, 0x282828, 0x282a36]) }
pub fn term_fg() -> u32 { pick([0xc8ccd4, 0xcccccc, 0x383a42, 0xc6d4e8, 0x2e3138, 0x23262c, 0xd8dee9, 0xebdbb2, 0xf8f8f2]) }

/// The terminal's 16 colors.
pub fn ansi(ix: usize) -> u32 {
    const DARK: [u32; 16] = [
        0x1d1f21, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd7dae0,
        0x5c6370, 0xff7a85, 0xb5e890, 0xffd68a, 0x82c4ff, 0xde9bf0, 0x7fd4dd, 0xffffff,
    ];
    const LIGHT: [u32; 16] = [
        0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0x8e9097,
        0x696c77, 0xca1243, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0x383a42,
    ];
    const ARCTIC: [u32; 16] = [
        0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
        0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
    ];
    const EMBER: [u32; 16] = [
        0x282828, 0xdc271f, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
        0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
    ];
    const GRAPE: [u32; 16] = [
        0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
        0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
    ];
    // Light colors, a shade darker for Soft Gray's gray background.
    const SOFT: [u32; 16] = [
        0x383a42, 0xe14637, 0x468c45, 0xa87301, 0x3e76f2, 0xa626a4, 0x0184bc, 0x6a6d75,
        0x55585f, 0xca1243, 0x468c45, 0xa87301, 0x3e76f2, 0xa626a4, 0x0184bc, 0x2e3138,
    ];
    // Darker still for Stone Gray.
    const STONE: [u32; 16] = [
        0x23262c, 0xa8281c, 0x2f6a2e, 0x7a5400, 0x2b55b5, 0x86208a, 0x006487, 0x4f525a,
        0x3f424a, 0x9e1035, 0x2f6a2e, 0x7a5400, 0x2b55b5, 0x86208a, 0x006487, 0x15171c,
    ];
    pick([DARK, DARK, LIGHT, DARK, SOFT, STONE, ARCTIC, EMBER, GRAPE])[ix % 16]
}

themed! {
    /// Frame color of the focused pane (zellij-style).
    frame_focused: 0x98c379, 0x0078d4, 0x005fb8, 0x5ccfe6, 0x4a6fa5, 0x3d5f94, 0x88c0d0, 0xd79921, 0xbd93f9;
    frame: 0x2f343e, 0x2b2b2b, 0xd8d8d8, 0x1e3150, 0xbfc1c7, 0xaeb1b8, 0x3b4252, 0x3c3836, 0x343746;
    bg_app: 0x0c0e11, 0x181818, 0xf0f0f0, 0x0b1524, 0xc9cacf, 0xb4b6bc, 0x242933, 0x1d2021, 0x1e1f29;
}

pub fn ui_text() -> Pixels {
    px(11.5)
}

pub fn row_h() -> Pixels {
    px(20.)
}

themed! {
    // Surfaces
    bg_editor: 0x111318, 0x1f1f1f, 0xffffff, 0x0f1b2d, 0xdcdde0, 0xc8cacf, 0x2e3440, 0x282828, 0x282a36;
    bg_side: 0x0c0e11, 0x181818, 0xf8f8f8, 0x0b1524, 0xd2d3d7, 0xbec0c6, 0x272c36, 0x1d2021, 0x21222c;
    bg_input: 0x171a20, 0x313131, 0xffffff, 0x13233a, 0xe4e5e8, 0xd0d2d6, 0x3b4252, 0x32302f, 0x343746;
    bg_popup: 0x15181e, 0x202020, 0xffffff, 0x122036, 0xe6e7ea, 0xd2d4d8, 0x343a46, 0x32302f, 0x2d2f3d;
    border: 0x262a33, 0x2b2b2b, 0xe5e5e5, 0x1e3150, 0xbfc1c7, 0xaeb1b8, 0x3b4252, 0x3c3836, 0x343746;
    border_input: 0x343a46, 0x3c3c3c, 0xcecece, 0x2a4266, 0xa9acb4, 0x989ba4, 0x4c566a, 0x504945, 0x44475a;
    focus_border: 0x98c379, 0x0078d4, 0x005fb8, 0x5ccfe6, 0x4a6fa5, 0x3d5f94, 0x88c0d0, 0xd79921, 0xbd93f9;

    // Text
    fg: 0xc8ccd4, 0xcccccc, 0x3b3b3b, 0xc6d4e8, 0x2e3138, 0x23262c, 0xd8dee9, 0xebdbb2, 0xf8f8f2;
    fg_bright: 0xeceff4, 0xffffff, 0x1f1f1f, 0xeef4fc, 0x16181d, 0x0f1115, 0xeceff4, 0xfbf1c7, 0xffffff;
    fg_dim: 0x8b919c, 0x9d9d9d, 0x616161, 0x8aa0bf, 0x51555f, 0x43474f, 0xa3abbc, 0xbdae93, 0xb8bacb;
    fg_muted: 0x5e6573, 0x6e7681, 0x8b8b8b, 0x5b7090, 0x727681, 0x5f636c, 0x78859b, 0x928374, 0x7a7fa0;

    // Accents
    accent: 0x61afef, 0x0078d4, 0x005fb8, 0x4fa8ff, 0x3f67a8, 0x345a96, 0x81a1c1, 0x428083, 0xbd93f9;
    accent_hover: 0x7cbcf2, 0x026ec1, 0x0258a8, 0x6cb8ff, 0x35599a, 0x2c4e85, 0x88c0d0, 0x548155, 0xcaa9fa;
    hover: 0x1b1f27, 0x2a2d2e, 0xebebeb, 0x15253d, 0xcdced3, 0xbcbec4, 0x353b49, 0x32302f, 0x30323f;
    selected_focused: 0x253447, 0x04395e, 0xd6ebff, 0x1d3a63, 0xb9c8e0, 0xa8b7d0, 0x3f4d66, 0x45403a, 0x44475a;
    selected: 0x20242c, 0x37373d, 0xe4e6f1, 0x172a45, 0xc6c9d1, 0xb4b8c0, 0x3b4252, 0x3c3836, 0x383a4a;
    badge: 0x3a404c, 0x616161, 0xcccccc, 0x2a4266, 0xaeb1b9, 0x9a9ea7, 0x4c566a, 0x504945, 0x44475a;

    // Editor
    selection: 0x2d4b6e, 0x264f78, 0xadd6ff, 0x1f4a7a, 0xb3c6e6, 0xa0b4d6, 0x434c5e, 0x504945, 0x44475a;
    selection_inactive: 0x2a2f38, 0x3a3d41, 0xe5ebf1, 0x1a3150, 0xc8ccd4, 0xb4b8c0, 0x3b4252, 0x3c3836, 0x3a3c4e;
    cursor: 0xaeafad, 0xaeafad, 0x000000, 0x80cbff, 0x1f2329, 0x15171c, 0xd8dee9, 0xebdbb2, 0xf8f8f2;
    line_number: 0x4b515c, 0x6e7681, 0x9e9e9e, 0x3e5577, 0x8a8e98, 0x6f737c, 0x566177, 0x7c6f64, 0x6272a4;
    indent_guide: 0x262a32, 0x404040, 0xe0e0e0, 0x1a2c47, 0xc6c8cd, 0xb4b6bc, 0x3b4252, 0x3c3836, 0x343746;
    bracket_match_border: 0x6b7280, 0x888888, 0xb9b9b9, 0x4d6a92, 0x9a9ea8, 0x878b94, 0x616e88, 0x7c6f64, 0x6272a4;
    line_number_active: 0xcccccc, 0xcccccc, 0x171184, 0xc6d4e8, 0x2e3138, 0x23262c, 0xd8dee9, 0xfabd2f, 0xf8f8f2;

    // Git
    git_modified: 0xe2c08d, 0xe2c08d, 0x895503, 0xe2c08d, 0x8a5d00, 0x7a5200, 0xebcb8b, 0xfabd2f, 0xf1fa8c;
    git_added: 0x73c991, 0x73c991, 0x587c0c, 0x73c991, 0x3f7a1c, 0x336a14, 0xa3be8c, 0xb8bb26, 0x50fa7b;
    git_deleted: 0xc74e39, 0xc74e39, 0xad0707, 0xf07178, 0xb3261e, 0x9e1f18, 0xbf616a, 0xfb4934, 0xff5555;
    git_conflict: 0xe4676b, 0xe4676b, 0xad0707, 0xe4676b, 0xb3261e, 0x9e1f18, 0xd08770, 0xfe8019, 0xffb86c;
    git_ignored: 0x8c8c8c, 0x8c8c8c, 0x8e8e90, 0x6a7f9e, 0x80838c, 0x6a6d76, 0x616e88, 0x7c6f64, 0x6272a4;
    gutter_added: 0x2ea043, 0x2ea043, 0x2ea043, 0x2ea043, 0x2ea043, 0x2a8a3c, 0xa3be8c, 0xb8bb26, 0x50fa7b;
    gutter_modified: 0x0078d4, 0x0078d4, 0x0078d4, 0x4fa8ff, 0x0078d4, 0x0067b8, 0x81a1c1, 0x83a598, 0x8be9fd;
    gutter_deleted: 0xf85149, 0xf85149, 0xf85149, 0xf85149, 0xf85149, 0xd63c34, 0xbf616a, 0xfb4934, 0xff5555;
}

themed_a! {
    line_highlight: 0xffffff0a, 0xffffff0a, 0x0000000a, 0xffffff0a, 0x0000000a, 0x0000000d, 0xffffff0a, 0xffffff0a, 0xffffff0a;
    bracket_match: 0x5c637033, 0x5c637033, 0x0064001a, 0x5c637033, 0x0064001a, 0x0064001a, 0x616e8844, 0x7c6f6444, 0x6272a444;
    find_match: 0xea5c0055, 0xea5c0055, 0xf8c84466, 0xea5c0055, 0xf8c84466, 0xe0b03466, 0xebcb8b44, 0xfabd2f44, 0xf1fa8c44;
    scrollbar: 0x79797966, 0x79797966, 0x64646466, 0x4d6a9266, 0x5a5e6866, 0x50545e66, 0x616e8866, 0x7c6f6466, 0x6272a466;

    // Diff
    diff_ins_line: 0x2ea04326, 0x2ea04326, 0x2ea04322, 0x2ea04326, 0x2ea04322, 0x2ea04326, 0xa3be8c22, 0xb8bb2622, 0x50fa7b1f;
    diff_del_line: 0xf8514926, 0xf8514926, 0xf8514922, 0xf8514926, 0xf8514922, 0xf8514926, 0xbf616a26, 0xfb493422, 0xff55551f;
    diff_ins_char: 0x2ea04366, 0x2ea04366, 0x2ea04355, 0x2ea04366, 0x2ea04355, 0x2ea04359, 0xa3be8c55, 0xb8bb2655, 0x50fa7b4d;
    diff_del_char: 0xf8514966, 0xf8514966, 0xf8514955, 0xf8514966, 0xf8514955, 0xf8514959, 0xbf616a5c, 0xfb493455, 0xff55554d;
    diff_filler: 0xffffff06, 0xffffff06, 0x00000008, 0xffffff06, 0x00000008, 0x0000000a, 0xffffff06, 0xffffff06, 0xffffff06;
}

/// What a syntax-highlighting capture is, for picking its color.
#[derive(Clone, Copy, Debug)]
pub enum Syntax {
    Comment, Builtin, Param, FnBuiltin, PunctSpecial, StringSpecial, Keyword, String, Number,
    Function, Type, Property, Attribute, Operator, Punct, Plain,
}

pub fn syntax(s: Syntax) -> Hsla {
    use Syntax::*;
    // One row per theme (in the order of `Theme::ALL`), each in the order of `Syntax`.
    let row: [u32; 16] = pick([
        [0x5c6370, 0xe5c07b, 0xd19a66, 0x56b6c2, 0xc678dd, 0x56b6c2, 0xc678dd, 0x98c379, 0xd19a66, 0x61afef, 0xe5c07b, 0xe06c75, 0xd19a66, 0x56b6c2, 0x8b919c, 0xc8ccd4],
        [0x6a9955, 0x4fc1ff, 0x9cdcfe, 0xdcdcaa, 0xc586c0, 0xd7ba7d, 0xc586c0, 0xce9178, 0xb5cea8, 0xdcdcaa, 0x4ec9b0, 0x9cdcfe, 0x9cdcfe, 0xd4d4d4, 0xd4d4d4, 0xd4d4d4],
        [0x93949b, 0x9d6c01, 0x986801, 0x017db2, 0xa626a4, 0x017db2, 0xa626a4, 0x428441, 0x986801, 0x326ef1, 0x9d6c01, 0xde3323, 0x986801, 0x017db2, 0x383a42, 0x383a42],
        [0x5f7a9e, 0x2ac3de, 0xe0af68, 0x2ac3de, 0xbb9af7, 0x89ddff, 0xbb9af7, 0x9ece6a, 0xff9e64, 0x7aa2f7, 0x2ac3de, 0x73daca, 0xe0af68, 0x89ddff, 0x9aa5ce, 0xc0caf5],
        // Soft Gray: One Light's hues, a shade darker for the gray background.
        [0x797d87, 0x8a5509, 0x825800, 0x0e6a88, 0x8b3d9a, 0x0e6a88, 0x8b3d9a, 0x386c33, 0x8d5300, 0x2f5fb3, 0x835a00, 0xac3846, 0x825800, 0x0e6a88, 0x3a3d45, 0x2e3138],
        // Stone Gray: darker again, for the darker gray.
        [0x5f636c, 0x6d4304, 0x684600, 0x08566f, 0x6f2d79, 0x08566f, 0x6f2d79, 0x2c582a, 0x704200, 0x244c93, 0x684700, 0x8b2d39, 0x684600, 0x08566f, 0x2e3137, 0x23262c],
        // Arctic (Nord-style)
        [0x6f7d98, 0x81a1c1, 0xd8dee9, 0x88c0d0, 0x81a1c1, 0xebcb8b, 0x81a1c1, 0xa3be8c, 0xb691af, 0x88c0d0, 0x8fbcbb, 0xd8dee9, 0x8fbcbb, 0x81a1c1, 0xa3abbc, 0xd8dee9],
        // Ember (Gruvbox-style)
        [0x928374, 0xd3869b, 0x83a598, 0x8ec07c, 0xfe8019, 0xfe8019, 0xfb5441, 0xb8bb26, 0xd3869b, 0xfabd2f, 0x8ec07c, 0x83a598, 0x8ec07c, 0xfe8019, 0xa89984, 0xebdbb2],
        // Grape (Dracula-style)
        [0x6272a4, 0xbd93f9, 0xffb86c, 0x8be9fd, 0xff79c6, 0xff79c6, 0xff79c6, 0xf1fa8c, 0xbd93f9, 0x50fa7b, 0x8be9fd, 0x8be9fd, 0x50fa7b, 0xff79c6, 0xb8bacb, 0xf8f8f2],
    ]);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG contrast ratio between two colors (1 to 21).
    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let lum = |c: Hsla| {
            let c = c.to_rgb();
            let f = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn every_theme_stays_readable() {
        let mut bad = Vec::new();
        for t in Theme::ALL {
            set(t);
            let mut need = |what: &str, fg: Hsla, bg: Hsla, min: f32| {
                let r = contrast(fg, bg);
                if r < min {
                    bad.push(format!("{}: {what} {r:.2} < {min}", t.label()));
                }
            };
            for (name, bg) in [("sidebar", bg_side()), ("editor", bg_editor()), ("popup", bg_popup())] {
                need(&format!("text on {name}"), fg(), bg, 7.);
                need(&format!("bright text on {name}"), fg_bright(), bg, 7.);
                need(&format!("dim text on {name}"), fg_dim(), bg, 4.5);
                need(&format!("muted text on {name}"), fg_muted(), bg, 3.);
            }
            // Selected and hovered rows: a tint over the above, secondary text a little lower.
            for (name, bg) in [("selected row", selected_focused()), ("hovered row", hover())] {
                need(&format!("text on {name}"), fg(), bg, 6.);
                need(&format!("dim text on {name}"), fg_dim(), bg, 3.5);
                need(&format!("muted text on {name}"), fg_muted(), bg, 2.);
            }
            for (name, c) in [("modified", git_modified()), ("added", git_added()), ("deleted", git_deleted()), ("conflict", git_conflict())] {
                need(&format!("git {name} on sidebar"), c, bg_side(), 3.);
            }
            need("accent text on editor", accent(), bg_editor(), 3.);
            need("text on accent", on(accent()), accent(), 4.5);
            need("text on accent hover", on(accent_hover()), accent_hover(), 4.5);
            need("text on badge", on(badge()), badge(), 4.5);
            need("line numbers", line_number(), bg_editor(), 2.);
            need("current line number", line_number_active(), bg_editor(), 4.5);
            need("cursor", cursor(), bg_editor(), 4.5);
            for s in [Syntax::Comment, Syntax::Keyword, Syntax::String, Syntax::Number, Syntax::Function, Syntax::Type, Syntax::Property, Syntax::Operator, Syntax::Punct, Syntax::Plain, Syntax::Builtin, Syntax::Param] {
                need(&format!("code {s:?}"), syntax(s), bg_editor(), if matches!(s, Syntax::Comment | Syntax::Punct) { 3. } else { 4.5 });
            }
            for i in [1, 2, 3, 4, 5, 6] {
                need(&format!("terminal color {i}"), c(ansi(i)), c(term_bg()), 3.);
            }
            need("terminal text", c(term_fg()), c(term_bg()), 7.);
        }
        set(Theme::Default);
        assert!(bad.is_empty(), "{} too faint:\n{}", bad.len(), bad.join("\n"));
    }
}
