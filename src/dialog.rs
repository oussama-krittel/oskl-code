//! In-app confirmation dialog, drawn in the editor's own theme (instead of the macOS alert):
//! a card over the dimmed window with a title, details and buttons. Enter picks the
//! highlighted button, Esc cancels, Tab / ←→ move between buttons.

use gpui::{App, Window, actions};
use std::rc::Rc;

actions!(dialog, [DialogConfirm, DialogCancel, DialogNext, DialogPrev]);

pub fn bind_keys() {
    let c = Some("Dialog");
    crate::keybindings::collect([
        crate::keybindings::kb("enter", DialogConfirm, c),
        crate::keybindings::kb("escape", DialogCancel, c),
        crate::keybindings::kb("tab", DialogNext, c),
        crate::keybindings::kb("right", DialogNext, c),
        crate::keybindings::kb("shift-tab", DialogPrev, c),
        crate::keybindings::kb("left", DialogPrev, c),
    ]);
}

/// Called with the index of the chosen button (never on Cancel).
pub type Answer = Rc<dyn Fn(usize, &mut Window, &mut App)>;

#[derive(Clone)]
pub struct Ask {
    pub title: String,
    pub detail: String,
    /// Action buttons, the main one first (it's highlighted and sits rightmost).
    pub buttons: Vec<String>,
    /// The main button destroys something (drawn red).
    pub danger: bool,
    /// Enter means Cancel until another button is picked (for warnings you should read).
    pub cancel_default: bool,
    pub on_answer: Answer,
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ask").field("title", &self.title).field("buttons", &self.buttons).finish()
    }
}
