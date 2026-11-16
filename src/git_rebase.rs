//! Interactive rebase, a wide dialog over the window: the commits from the one picked up to
//! HEAD, oldest first, each picked, reworded, squashed, fixed up or dropped, and moved.

use super::RefsTree;
use crate::editor::{Editor, EditorMode};
use crate::git;
use crate::request::Request;
use crate::ui::px;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;

const ACTIONS: [&str; 5] = ["pick", "reword", "squash", "fixup", "drop"];

fn button(id: &'static str, label: &'static str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex_none()
        .px(px(12.))
        .h(px(26.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .cursor_pointer()
        .when(primary, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).font_weight(FontWeight::SEMIBOLD).hover(|s| s.bg(theme::accent_hover())))
        .when(!primary, |d| d.border_1().border_color(theme::border_input()).hover(|s| s.bg(theme::hover())))
        .child(label)
}

/// A row being dragged by its handle.
#[derive(Clone)]
struct DraggedStep(usize);

fn joins(action: usize) -> bool {
    matches!(ACTIONS[action], "squash" | "fixup")
}

impl Plan {
    /// The first commit kept can't join one before it: there is none.
    fn first_joins(&self) -> bool {
        self.steps.iter().find(|s| ACTIONS[s.action] != "drop").is_some_and(|s| joins(s.action))
    }

    /// "3 commits become 2".
    fn summary(&self) -> String {
        let n = self.steps.len();
        format!("{n} commit{} become{} {}", if n == 1 { "" } else { "s" }, if n == 1 { "s" } else { "" }, self.kept())
    }

    /// How many commits the plan leaves: those picked or reworded.
    fn kept(&self) -> usize {
        self.steps.iter().filter(|s| matches!(ACTIONS[s.action], "pick" | "reword")).count()
    }
}

pub struct Step {
    pub sha: String,
    pub action: usize,
    /// The subject, editable (it's the new message when rewording).
    pub message: Entity<Editor>,
    original: String,
}

pub struct Plan {
    pub first: String,
    pub steps: Vec<Step>,
}

impl RefsTree {
    pub(super) fn open_rebase(&mut self, first: &str, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        match git::rebase_commits(&root, first) {
            Ok(commits) if !commits.is_empty() => {
                let steps = commits
                    .into_iter()
                    .map(|(sha, subject)| {
                        let message = cx.new(|cx| {
                            let mut ed = Editor::new(EditorMode::SingleLine, cx);
                            ed.font_size = 12.;
                            ed.set_text(&subject, cx);
                            // The start of the message in view, not its end.
                            ed.go_to(0, 0, cx);
                            ed
                        });
                        Step { sha, action: 0, message, original: subject }
                    })
                    .collect();
                self.rebase = Some(Plan { first: first.to_string(), steps });
            }
            Ok(_) => cx.emit(Request::Error("Nothing to rebase there".into())),
            Err(e) => cx.emit(Request::Error(format!("Interactive rebase: {e}"))),
        }
        cx.notify();
    }

    pub fn move_rebase_step(&mut self, ix: usize, up: bool, cx: &mut Context<Self>) {
        let Some(plan) = self.rebase.as_mut() else { return };
        let to = if up { ix.checked_sub(1) } else { Some(ix + 1).filter(|t| *t < plan.steps.len()) };
        if let Some(to) = to {
            self.move_rebase_step_to(ix, to, cx);
        }
    }

    /// Puts the row `from` where the row `to` is (dragging).
    pub fn move_rebase_step_to(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(plan) = self.rebase.as_mut() else { return };
        if from != to && from < plan.steps.len() && to < plan.steps.len() {
            let step = plan.steps.remove(from);
            plan.steps.insert(to, step);
            cx.notify();
        }
    }

    pub fn start_rebase(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = self.rebase.take() else { return };
        // A squash or fixup needs a commit before it to land in.
        if plan.first_joins() {
            cx.emit(Request::Error("The first commit kept can't be squashed or fixed up: there's nothing before it to join".into()));
            self.rebase = Some(plan);
            return cx.notify();
        }
        let steps: Vec<(String, String, Option<String>)> = plan
            .steps
            .iter()
            .map(|s| {
                let text = s.message.read(cx).text();
                let action = if ACTIONS[s.action] == "pick" && text.trim() != s.original { "reword" } else { ACTIONS[s.action] };
                (action.to_string(), s.sha.clone(), (action == "reword").then(|| text.trim().to_string()))
            })
            .collect();
        let first = plan.first;
        self.repo.update(cx, |r, cx| r.run_op("Interactive rebase", cx, move |root| git::rebase_interactive(root, &first, &steps)));
        cx.notify();
    }

    /// The dialog and the dimmed window behind it, as big as the window.
    pub(super) fn render_rebase(&self, vp: Size<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(plan) = &self.rebase else { return div().into_any_element() };
        let branch = self.repo.read(cx).branch_label();
        let (total, bad) = (plan.steps.len(), plan.first_joins());
        let dialog = div()
            .id("rebase-plan")
            .debug_selector(|| "rebase-plan".into())
            .occlude()
            .w(px(640.))
            .max_w(vp.width - gpui::px(24.))
            .max_h(vp.height * 0.82)
            .flex()
            .flex_col()
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(10.))
            .shadow_lg()
            .text_size(px(12.))
            .child(
                div()
                    .px(px(16.))
                    .pt(px(14.))
                    .text_size(px(14.))
                    .text_color(theme::fg_bright())
                    .font_weight(FontWeight::SEMIBOLD)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(format!("Interactive rebase of {branch}, from {}", &plan.first[..plan.first.len().min(7)])),
            )
            .child(div().px(px(16.)).pt(px(4.)).pb(px(8.)).text_color(theme::fg_muted()).child("Oldest first. Drag a row by its handle, or use the arrows, to reorder. Click an action to change it; edit a message to reword it."))
            .child(
                div().id("rebase-steps").flex().flex_col().gap(px(2.)).px(px(10.)).min_h_0().overflow_y_scroll().children(plan.steps.iter().enumerate().map(|(i, s)| {
                    let action = ACTIONS[s.action];
                    let color = match action {
                        "drop" => theme::git_deleted(),
                        "squash" | "fixup" => theme::git_modified(),
                        "reword" => theme::accent(),
                        _ => theme::fg_dim(),
                    };
                    let label = s.message.read(cx).text();
                    div()
                        .id(("rebase-row", i))
                        .debug_selector(move || format!("rebase-row-{i}"))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(30.))
                        .pr(px(6.))
                        .pl(px(if joins(s.action) { 26. } else { 6. }))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(gpui::transparent_black())
                        .hover(|s| s.bg(theme::hover()))
                        .drag_over::<DraggedStep>(|s, _, _, _| s.border_color(theme::accent()))
                        .on_drop(cx.listener(move |t, d: &DraggedStep, _, cx| t.move_rebase_step_to(d.0, i, cx)))
                        .child(
                            div()
                                .id(("rebase-grip", i))
                                .debug_selector(move || format!("rebase-grip-{i}"))
                                .flex_none()
                                .cursor(CursorStyle::OpenHand)
                                .tooltip(ui::tooltip("Drag to reorder"))
                                .on_drag(DraggedStep(i), move |_, _, _, cx| cx.new(|_| ui::DragChip(label.clone())))
                                .child(ui::small_icon("grip", theme::fg_muted())),
                        )
                        .child(
                            div()
                                .id(("rebase-action", i))
                                .debug_selector(move || format!("rebase-action-{i}"))
                                .flex_none()
                                .w(px(64.))
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .font_family(theme::MONO_FONT)
                                .rounded(px(5.))
                                .border_1()
                                .border_color(color)
                                .text_color(color)
                                .cursor_pointer()
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    if let Some(step) = p.rebase.as_mut().and_then(|p| p.steps.get_mut(i)) {
                                        step.action = (step.action + 1) % ACTIONS.len();
                                        cx.notify();
                                    }
                                }))
                                .child(action),
                        )
                        .child(div().flex_none().font_family(theme::MONO_FONT).text_color(theme::fg_muted()).child(s.sha[..7].to_string()))
                        .child(div().id(("rebase-message", i)).debug_selector(move || format!("rebase-message-{i}")).flex_1().min_w_0().overflow_hidden().when(action == "drop", |d| d.opacity(0.5)).child(s.message.clone()))
                        .child(ui::icon_button(("rebase-up", i), "arrow-up").debug_selector(move || format!("rebase-up-{i}")).size(px(20.)).when(i == 0, |d| d.opacity(0.3)).tooltip(ui::tooltip("Move earlier")).on_click(cx.listener(move |p, _, _, cx| p.move_rebase_step(i, true, cx))))
                        .child(ui::icon_button(("rebase-down", i), "arrow-down").debug_selector(move || format!("rebase-down-{i}")).size(px(20.)).when(i + 1 == total, |d| d.opacity(0.3)).tooltip(ui::tooltip("Move later")).on_click(cx.listener(move |p, _, _, cx| p.move_rebase_step(i, false, cx))))
                })),
            )
            .when(bad, |d| d.child(div().id("rebase-bad").debug_selector(|| "rebase-bad".into()).px(px(16.)).pt(px(8.)).text_color(theme::git_deleted()).child("The first commit kept has nothing before it to squash into. Move it down, or pick it.")))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .mt(px(10.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(div().id("rebase-count").debug_selector(|| "rebase-count".into()).mr_auto().text_color(theme::fg_muted()).child(plan.summary()))
                    .child(button("rebase-cancel", "Cancel", false).on_click(cx.listener(|p, _, _, cx| {
                        p.rebase = None;
                        cx.notify();
                    })))
                    .child(
                        button("rebase-start", "Start rebase", true)
                            .when(bad, |d| d.opacity(0.45).cursor_default())
                            .when(!bad, |d| d.on_click(cx.listener(|p, _, _, cx| p.start_rebase(cx)))),
                    ),
            );
        ui::dialog_scrim(vp, dialog).into_any_element()
    }
}

#[cfg(test)]
impl RefsTree {
    /// The dialog's count line and the messages in their current order.
    pub fn rebase_state(&self, cx: &App) -> Option<(String, Vec<String>)> {
        let p = self.rebase.as_ref()?;
        Some((p.summary(), p.steps.iter().map(|s| s.message.read(cx).text()).collect()))
    }
}
