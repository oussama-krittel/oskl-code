//! Persistent user settings in ~/.config/kite/settings (simple `key = value` lines).

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    pub sidebar_right: bool,
    pub font_size: f32,
    pub blame_default: bool,
    pub autosave: bool,
    pub checks: bool,
    pub format_on_save: bool,
    pub recent_folders: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sidebar_right: false, font_size: 12., blame_default: false, autosave: true, checks: true, format_on_save: true, recent_folders: Vec::new() }
    }
}

fn path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/kite/settings"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return s };
        let mut version = 1;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "sidebar" => s.sidebar_right = v == "right",
                "font_size" => s.font_size = v.parse().unwrap_or(12.),
                "v" => version = v.parse().unwrap_or(1),
                "blame" => s.blame_default = v == "on",
                "autosave" => s.autosave = v != "off",
                "checks" => s.checks = v != "off",
                "formatonsave" => s.format_on_save = v != "off",
                "recent" => s.recent_folders.push(PathBuf::from(v)),
                _ => {}
            }
        }
        // v1 stored 13 as the default editor size; the new default is smaller.
        if version < 2 && s.font_size == 13. {
            s.font_size = 12.;
        }
        s
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let mut out = format!(
            "v = 2\nsidebar = {}\nfont_size = {}\nblame = {}\nautosave = {}\nchecks = {}\nformatonsave = {}\n",
            if self.sidebar_right { "right" } else { "left" },
            self.font_size,
            if self.blame_default { "on" } else { "off" },
            if self.autosave { "on" } else { "off" },
            if self.checks { "on" } else { "off" },
            if self.format_on_save { "on" } else { "off" },
        );
        for r in &self.recent_folders {
            out.push_str(&format!("recent = {}\n", r.display()));
        }
        std::fs::write(p, out).ok();
    }

    pub fn add_recent(&mut self, folder: PathBuf) {
        self.recent_folders.retain(|f| f != &folder);
        self.recent_folders.insert(0, folder);
        self.recent_folders.truncate(15);
    }
}
