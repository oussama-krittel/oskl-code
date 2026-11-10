//! Text in the database views that can be selected by dragging across it: keys and values of
//! documents (List, JSON) and table cells, over as many lines and cells as the drag covers.
//! Each piece of text registers itself as it's drawn; ⌘C copies the selection in reading
//! order (cells joined by tabs, lines by new lines).

use gpui::{
    App, Bounds, Element, ElementId, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, SharedString, StyledText, TextLayout, TextStyleRefinement, Window, fill,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Where a piece of text sits in reading order: its line, then its place on the line.
pub type Order = (u64, u32);

struct Piece {
    text: SharedString,
    layout: TextLayout,
    /// Put before it when it follows another piece of the same line ("" for a key and its value, a tab between cells).
    join: &'static str,
}

#[derive(Default)]
pub struct State {
    pieces: BTreeMap<Order, Piece>,
    anchor: Option<(Order, usize)>,
    head: Option<(Order, usize)>,
    dragging: bool,
}

/// A view's selection, shared by its pieces of text.
#[derive(Clone, Default)]
pub struct Selection(Rc<RefCell<State>>);

impl Selection {
    /// Forgets the selection (and the pieces drawn: the rows changed).
    pub fn reset(&self) {
        *self.0.borrow_mut() = State::default();
    }

    pub fn clear(&self) {
        let mut s = self.0.borrow_mut();
        s.anchor = None;
        s.head = None;
        s.dragging = false;
    }

    /// Something is selected (not just clicked).
    pub fn has_selection(&self) -> bool {
        let s = self.0.borrow();
        matches!((s.anchor, s.head), (Some(a), Some(h)) if a != h)
    }

    fn range(s: &State) -> Option<((Order, usize), (Order, usize))> {
        let (a, h) = (s.anchor?, s.head?);
        (a != h).then(|| if a <= h { (a, h) } else { (h, a) })
    }

    /// The place in the text under `pos`: the piece on that line the pointer is over (or the
    /// nearest one, at its start or end). None between lines.
    fn locate(s: &State, pos: Point<Pixels>) -> Option<(Order, usize)> {
        let mut best: Option<(Pixels, Order, usize)> = None;
        for (order, p) in &s.pieces {
            let b = p.layout.bounds();
            if pos.y < b.top() || pos.y >= b.bottom() {
                continue;
            }
            let (dist, at) = if pos.x < b.left() {
                (b.left() - pos.x, 0)
            } else if pos.x > b.right() {
                (pos.x - b.right(), p.text.len())
            } else {
                (Pixels::ZERO, index_at(&p.layout, pos))
            };
            if best.as_ref().is_none_or(|(d, _, _)| dist < *d) {
                best = Some((dist, *order, at));
            }
        }
        best.map(|(_, o, i)| (o, i))
    }

    /// A press: a selection starts at the text there (or the nearest on that line).
    /// Twice selects the word there, three times the whole line, as in a text file.
    pub fn press(&self, ev: &MouseDownEvent, window: &mut Window) {
        let mut s = self.0.borrow_mut();
        let Some(at) = Self::locate(&s, ev.position) else { return };
        match ev.click_count {
            1 => {
                s.anchor = Some(at);
                s.head = Some(at);
                s.dragging = true;
            }
            2 => {
                let Some(p) = s.pieces.get(&at.0) else { return };
                let (a, b) = word_at(&p.text, at.1);
                s.anchor = Some((at.0, a));
                s.head = Some((at.0, b));
                s.dragging = false;
            }
            _ => {
                let line = at.0.0;
                let mut on_line = s.pieces.range((line, 0)..=(line, u32::MAX));
                let first = on_line.next().map(|(o, _)| *o);
                let last = s.pieces.range((line, 0)..=(line, u32::MAX)).next_back().map(|(o, p)| (*o, p.text.len()));
                if let (Some(f), Some((l, len))) = (first, last) {
                    s.anchor = Some((f, 0));
                    s.head = Some((l, len));
                }
                s.dragging = false;
            }
        }
        window.refresh();
    }

    /// The selected text, as it reads.
    pub fn text(&self) -> String {
        let s = self.0.borrow();
        let Some((start, end)) = Self::range(&s) else { return String::new() };
        let mut out = String::new();
        let mut last_line = None;
        for (order, p) in s.pieces.range(start.0..=end.0) {
            let from = if *order == start.0 { start.1.min(p.text.len()) } else { 0 };
            let to = if *order == end.0 { end.1.min(p.text.len()) } else { p.text.len() };
            if let Some(l) = last_line {
                out.push_str(if l == order.0 { p.join } else { "\n" });
            }
            last_line = Some(order.0);
            if from < to && p.text.is_char_boundary(from) && p.text.is_char_boundary(to) {
                out.push_str(&p.text[from..to]);
            }
        }
        out
    }

    /// A piece of text that takes part in the selection.
    pub fn text_piece(&self, order: Order, text: impl Into<SharedString>, color: Hsla, bold: bool) -> SelText {
        let text = text.into();
        SelText { sel: self.clone(), order, styled: StyledText::new(text.clone()), text, style: TextStyleRefinement { color: Some(color), font_weight: bold.then_some(FontWeight::SEMIBOLD), ..Default::default() }, join: "" }
    }
}

pub struct SelText {
    sel: Selection,
    order: Order,
    text: SharedString,
    styled: StyledText,
    style: TextStyleRefinement,
    join: &'static str,
}

impl SelText {
    /// What separates it from the piece before it on the same line, when copied.
    pub fn join(mut self, s: &'static str) -> Self {
        self.join = s;
        self
    }
}

impl IntoElement for SelText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// The word around a byte index: letters, digits and _ (or just the character there).
fn word_at(text: &str, at: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = at.min(text.len());
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let Some(i) = chars.iter().position(|(b, _)| *b >= at).or(chars.len().checked_sub(1)) else { return (at, at) };
    let i = if i > 0 && (i == chars.len() || !is_word(chars[i].1)) && is_word(chars[i - 1].1) { i - 1 } else { i };
    if !is_word(chars[i].1) {
        let end = chars.get(i + 1).map_or(text.len(), |c| c.0);
        return (chars[i].0, end);
    }
    let mut a = i;
    while a > 0 && is_word(chars[a - 1].1) {
        a -= 1;
    }
    let mut b = i;
    while b + 1 < chars.len() && is_word(chars[b + 1].1) {
        b += 1;
    }
    (chars[a].0, chars.get(b + 1).map_or(text.len(), |c| c.0))
}

/// The text index under `pos` (its end when past it).
fn index_at(layout: &TextLayout, pos: Point<Pixels>) -> usize {
    match layout.index_for_position(pos) {
        Ok(i) | Err(i) => i,
    }
}

impl Element for SelText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let style = self.style.clone();
        window.with_text_style(Some(style), |window| self.styled.request_layout(id, inspector_id, window, cx))
    }

    fn prepaint(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Hitbox {
        let style = self.style.clone();
        window.with_text_style(Some(style), |window| self.styled.prepaint(id, inspector_id, bounds, &mut (), window, cx));
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), hitbox: &mut Hitbox, window: &mut Window, cx: &mut App) {
        let layout = self.styled.layout().clone();
        let order = self.order;
        {
            let mut s = self.sel.0.borrow_mut();
            s.pieces.insert(order, Piece { text: self.text.clone(), layout: layout.clone(), join: self.join });
            // The selected part of this piece.
            if let Some((start, end)) = Selection::range(&s)
                && start.0 <= order
                && order <= end.0
            {
                let from = if start.0 == order { start.1 } else { 0 }.min(self.text.len());
                let to = if end.0 == order { end.1 } else { self.text.len() }.min(self.text.len());
                if from < to
                    && let (Some(a), Some(b)) = (layout.position_for_index(from), layout.position_for_index(to))
                {
                    let lh = layout.line_height();
                    let color = crate::theme::selection();
                    if a.y == b.y {
                        window.paint_quad(fill(Bounds::from_corners(a, gpui::point(b.x, b.y + lh)), color));
                    } else {
                        window.paint_quad(fill(Bounds::from_corners(a, gpui::point(bounds.right(), a.y + lh)), color));
                        if b.y > a.y + lh {
                            window.paint_quad(fill(Bounds::from_corners(gpui::point(bounds.left(), a.y + lh), gpui::point(bounds.right(), b.y)), color));
                        }
                        window.paint_quad(fill(Bounds::from_corners(gpui::point(bounds.left(), b.y), gpui::point(b.x, b.y + lh)), color));
                    }
                }
            }
        }
        let style = self.style.clone();
        window.with_text_style(Some(style), |window| self.styled.paint(id, inspector_id, bounds, &mut (), &mut (), window, cx));

        // A press on it starts a selection; a drag over it moves the selection's end.
        let (sel, hb) = (self.sel.clone(), hitbox.clone());
        window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, _| {
            if phase.bubble() && ev.button == MouseButton::Left && hb.is_hovered(window) {
                sel.press(ev, window);
            }
        });
        let sel = self.sel.clone();
        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, _| {
            if !phase.bubble() || !ev.dragging() {
                return;
            }
            let mut s = sel.0.borrow_mut();
            if !s.dragging {
                return;
            }
            // Every piece hears the move; they all find the same place.
            if let Some(at) = Selection::locate(&s, ev.position)
                && s.head != Some(at)
            {
                s.head = Some(at);
                window.refresh();
            }
        });
        let sel = self.sel.clone();
        window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, _| {
            if phase.bubble() && ev.button == MouseButton::Left {
                sel.0.borrow_mut().dragging = false;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, InteractiveElement as _, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext, div, px};

    struct V(Selection);
    impl Render for V {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let s = self.0.clone();
            let red = gpui::red();
            div()
                .size_full()
                .capture_any_mouse_down(move |_, _, _| s.clear())
                .child(div().flex().child(self.0.text_piece((0, 0), "key: ", red, true)).child(self.0.text_piece((0, 1), "alpha", red, false)))
                .child(div().flex().child(self.0.text_piece((1, 0), "a", red, false)).child(self.0.text_piece((1, 1), "b", red, false).join("\t")))
                .child(div().h(px(20.)).w(px(100.)))
        }
    }

    #[test]
    fn words_are_found() {
        let t = "status: 'COMPLETE_2', x";
        let w = |i: usize| {
            let (a, b) = word_at(t, i);
            &t[a..b]
        };
        assert_eq!(w(2), "status");
        assert_eq!(w(12), "COMPLETE_2");
        assert_eq!(w(6), "status", "just after a word: that word");
        assert_eq!(w(8), "'", "not in a word: the character there");
    }

    #[gpui::test]
    fn double_and_triple_clicks_select_a_word_and_a_line(cx: &mut TestAppContext) {
        let sel = Selection::default();
        let s2 = sel.clone();
        let (_, cx) = cx.add_window_view(|_, _| V(s2));
        cx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        let at = gpui::point(px(60.), px(6.));
        for n in 1..=2 {
            cx.simulate_event(MouseDownEvent { position: at, button: MouseButton::Left, click_count: n, ..Default::default() });
            cx.simulate_event(MouseUpEvent { position: at, button: MouseButton::Left, click_count: n, ..Default::default() });
        }
        assert_eq!(sel.text(), "alpha");
        cx.simulate_event(MouseDownEvent { position: at, button: MouseButton::Left, click_count: 3, ..Default::default() });
        cx.simulate_event(MouseUpEvent { position: at, button: MouseButton::Left, click_count: 3, ..Default::default() });
        assert_eq!(sel.text(), "key: alpha");
    }

    #[gpui::test]
    fn a_drag_selects_across_pieces_and_lines(cx: &mut TestAppContext) {
        let sel = Selection::default();
        let s2 = sel.clone();
        let (_, cx) = cx.add_window_view(|_, _| V(s2));
        cx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        let line = |cx: &mut gpui::VisualTestContext, y: f32, x: f32| {
            let _ = cx;
            gpui::point(px(x), px(y))
        };
        let (from, to) = (line(cx, 6., 1.), line(cx, 36., 400.));
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        cx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        assert_eq!(sel.text(), "key: alpha\na\tb");
        // A click elsewhere clears it.
        cx.simulate_click(gpui::point(px(50.), px(60.)), Modifiers::default());
        assert!(!sel.has_selection());
    }
}
