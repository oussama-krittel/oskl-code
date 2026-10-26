//! Colors and sizes, modelled on VS Code's "Dark Modern" theme.

use gpui::{Hsla, Pixels, px, rgb, rgba};

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn ca(hex_rgba: u32) -> Hsla {
    rgba(hex_rgba).into()
}

pub const UI_FONT: &str = "Menlo";
pub const MONO_FONT: &str = "Menlo";

pub const TERM_BG: u32 = 0x111318;
pub const TERM_FG: u32 = 0xc8ccd4;
/// Frame color of the focused pane (zellij-style).
pub fn frame_focused() -> Hsla { c(0x98c379) }
pub fn frame() -> Hsla { c(0x2f343e) }
pub fn bg_app() -> Hsla { c(0x0c0e11) }

pub fn ui_text() -> Pixels {
    px(11.5)
}

pub fn row_h() -> Pixels {
    px(20.)
}

// Surfaces
pub fn bg_editor() -> Hsla { c(0x111318) }
pub fn bg_side() -> Hsla { c(0x0c0e11) }
pub fn bg_input() -> Hsla { c(0x171a20) }
pub fn bg_popup() -> Hsla { c(0x15181e) }
pub fn border() -> Hsla { c(0x262a33) }
pub fn border_input() -> Hsla { c(0x343a46) }
pub fn focus_border() -> Hsla { c(0x98c379) }

// Text
pub fn fg() -> Hsla { c(0xc8ccd4) }
pub fn fg_bright() -> Hsla { c(0xeceff4) }
pub fn fg_dim() -> Hsla { c(0x8b919c) }
pub fn fg_muted() -> Hsla { c(0x5c6370) }

// Accents
pub fn accent() -> Hsla { c(0x61afef) }
pub fn accent_hover() -> Hsla { c(0x7cbcf2) }
pub fn hover() -> Hsla { c(0x1b1f27) }
pub fn selected_focused() -> Hsla { c(0x253447) }
pub fn selected() -> Hsla { c(0x20242c) }
pub fn badge() -> Hsla { c(0x3a404c) }

// Editor
pub fn selection() -> Hsla { c(0x2d4b6e) }
pub fn selection_inactive() -> Hsla { c(0x2a2f38) }
pub fn line_highlight() -> Hsla { ca(0xffffff0a) }
pub fn cursor() -> Hsla { c(0xaeafad) }
pub fn line_number() -> Hsla { c(0x4b515c) }
pub fn line_number_active() -> Hsla { c(0xcccccc) }
pub fn find_match() -> Hsla { ca(0xea5c0055) }
pub fn scrollbar() -> Hsla { ca(0x79797966) }

// Git
pub fn git_modified() -> Hsla { c(0xe2c08d) }
pub fn git_added() -> Hsla { c(0x73c991) }
pub fn git_deleted() -> Hsla { c(0xc74e39) }
pub fn git_conflict() -> Hsla { c(0xe4676b) }
pub fn git_ignored() -> Hsla { c(0x8c8c8c) }
pub fn gutter_added() -> Hsla { c(0x2ea043) }
pub fn gutter_modified() -> Hsla { c(0x0078d4) }
pub fn gutter_deleted() -> Hsla { c(0xf85149) }

// Diff
pub fn diff_ins_line() -> Hsla { ca(0x2ea04326) }
pub fn diff_del_line() -> Hsla { ca(0xf8514926) }
pub fn diff_ins_char() -> Hsla { ca(0x2ea04366) }
pub fn diff_del_char() -> Hsla { ca(0xf8514966) }
pub fn diff_filler() -> Hsla { ca(0xffffff06) }

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
