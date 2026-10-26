//! Text buffer: a rope plus one selection, undo/redo and all cursor motions.
//! Positions are char indices into the rope.

use ropey::Rope;
use std::ops::Range;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
}

impl Sel {
    pub fn at(pos: usize) -> Self {
        Self { anchor: pos, head: pos }
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
    Other,
}

struct Edit {
    at: usize,
    old: String,
    new: String,
}

struct Txn {
    edits: Vec<Edit>,
    before: Sel,
    after: Sel,
    kind: EditKind,
    time: Instant,
}

/// Rows touched by an edit: `start..=old_end` became `start..=new_end`.
#[derive(Clone, Copy, Debug)]
pub struct RowChange {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punct,
    Newline,
}

fn class(c: char) -> CharClass {
    if c == '\n' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' || c == '$' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

pub struct Buffer {
    pub rope: Rope,
    pub sel: Sel,
    pub goal_col: Option<usize>,
    version: u64,
    saved_version: u64,
    undo: Vec<Txn>,
    redo: Vec<Txn>,
    pub crlf: bool,
    pub indent: String,
    pub tab_width: usize,
    changes: Vec<RowChange>,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let crlf = text.contains("\r\n");
        let text = if crlf { text.replace("\r\n", "\n") } else { text.to_string() };
        let indent = detect_indent(&text);
        Self {
            rope: Rope::from_str(&text),
            sel: Sel::default(),
            goal_col: None,
            version: 0,
            saved_version: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            crlf,
            indent,
            tab_width: 4,
            changes: Vec::new(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    pub fn mark_saved(&mut self) {
        self.saved_version = self.version;
    }

    pub fn take_changes(&mut self) -> Vec<RowChange> {
        std::mem::take(&mut self.changes)
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn text_for_disk(&self) -> String {
        let text = self.rope.to_string();
        if self.crlf { text.replace('\n', "\r\n") } else { text }
    }

    /// Replaces the whole text (e.g. after an external change or a formatter),
    /// as one undoable step, keeping the cursor on the same line/column.
    pub fn set_text(&mut self, text: &str, undoable: bool) {
        let text = text.replace("\r\n", "\n");
        if text == self.text() {
            return;
        }
        let (row, col) = self.pos_to_rc(self.sel.head);
        if undoable {
            let len = self.len();
            self.replace(0..len, &text, None, EditKind::Other);
        } else {
            let old_end = self.line_count().saturating_sub(1);
            self.rope = Rope::from_str(&text);
            self.undo.clear();
            self.redo.clear();
            self.version += 1;
            self.changes.push(RowChange { start: 0, old_end, new_end: self.line_count().saturating_sub(1) });
        }
        let row = row.min(self.line_count().saturating_sub(1));
        self.sel = Sel::at(self.rc_to_pos(row, col));
    }

    pub fn len(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line(&self, row: usize) -> String {
        if row >= self.line_count() {
            return String::new();
        }
        let mut s = self.rope.line(row).to_string();
        if s.ends_with('\n') {
            s.pop();
        }
        s
    }

    pub fn line_len(&self, row: usize) -> usize {
        if row >= self.line_count() {
            return 0;
        }
        let line = self.rope.line(row);
        let n = line.len_chars();
        if n > 0 && line.char(n - 1) == '\n' { n - 1 } else { n }
    }

    pub fn line_start(&self, row: usize) -> usize {
        self.rope.line_to_char(row.min(self.line_count()))
    }

    pub fn pos_to_rc(&self, pos: usize) -> (usize, usize) {
        let pos = pos.min(self.len());
        let row = self.rope.char_to_line(pos);
        (row, pos - self.rope.line_to_char(row))
    }

    pub fn rc_to_pos(&self, row: usize, col: usize) -> usize {
        let row = row.min(self.line_count().saturating_sub(1));
        self.line_start(row) + col.min(self.line_len(row))
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        (pos < self.len()).then(|| self.rope.char(pos))
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        self.rope.slice(start..end).to_string()
    }

    // ------------------------------------------------------------ display columns

    /// Column on screen for a char column, with tabs expanded.
    pub fn display_col(&self, row: usize, col: usize) -> usize {
        let mut d = 0;
        for (i, ch) in self.line(row).chars().enumerate() {
            if i >= col {
                break;
            }
            d = if ch == '\t' { (d / self.tab_width + 1) * self.tab_width } else { d + 1 };
        }
        d
    }

    pub fn col_for_display(&self, row: usize, target: usize) -> usize {
        let mut d = 0;
        for (i, ch) in self.line(row).chars().enumerate() {
            let next = if ch == '\t' { (d / self.tab_width + 1) * self.tab_width } else { d + 1 };
            if next > target {
                return if target - d > (next - d) / 2 { i + 1 } else { i };
            }
            d = next;
        }
        self.line_len(row)
    }

    /// The line as displayed (tabs expanded) and the byte offset in it of every char column.
    pub fn display_line(&self, row: usize) -> (String, Vec<usize>) {
        let line = self.line(row);
        let mut out = String::with_capacity(line.len());
        let mut map = Vec::with_capacity(line.len() + 1);
        let mut d = 0;
        for ch in line.chars() {
            map.push(out.len());
            if ch == '\t' {
                let next = (d / self.tab_width + 1) * self.tab_width;
                for _ in d..next {
                    out.push(' ');
                }
                d = next;
            } else {
                out.push(ch);
                d += 1;
            }
        }
        map.push(out.len());
        (out, map)
    }

    // ------------------------------------------------------------ editing core

    pub fn replace(&mut self, range: Range<usize>, text: &str, sel_after: Option<Sel>, kind: EditKind) {
        let range = range.start.min(self.len())..range.end.min(self.len());
        let before = self.sel;
        let old = self.slice(range.clone());
        let start_row = self.rope.char_to_line(range.start);
        let old_end = self.rope.char_to_line(range.end);
        self.rope.remove(range.clone());
        self.rope.insert(range.start, text);
        let new_end = start_row + text.matches('\n').count();
        self.changes.push(RowChange { start: start_row, old_end, new_end });
        let after = sel_after.unwrap_or_else(|| Sel::at(range.start + text.chars().count()));
        self.sel = after;
        self.goal_col = None;
        self.version += 1;
        self.redo.clear();

        let edit = Edit { at: range.start, old, new: text.to_string() };
        let now = Instant::now();
        if let Some(last) = self.undo.last_mut()
            && kind != EditKind::Other
            && last.kind == kind
            && last.after == before
            && now.duration_since(last.time) < Duration::from_millis(1000)
            && !(kind == EditKind::Insert && text.contains('\n'))
        {
            last.edits.push(edit);
            last.after = after;
            last.time = now;
            return;
        }
        self.undo.push(Txn { edits: vec![edit], before, after, kind, time: now });
        if self.undo.len() > 2000 {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(txn) = self.undo.pop() else { return false };
        for e in txn.edits.iter().rev() {
            let n = e.new.chars().count();
            let start_row = self.rope.char_to_line(e.at);
            let old_end = self.rope.char_to_line(e.at + n);
            self.rope.remove(e.at..e.at + n);
            self.rope.insert(e.at, &e.old);
            self.changes.push(RowChange { start: start_row, old_end, new_end: start_row + e.old.matches('\n').count() });
        }
        self.sel = txn.before;
        self.version += 1;
        self.redo.push(txn);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(txn) = self.redo.pop() else { return false };
        for e in &txn.edits {
            let n = e.old.chars().count();
            let start_row = self.rope.char_to_line(e.at);
            let old_end = self.rope.char_to_line(e.at + n);
            self.rope.remove(e.at..e.at + n);
            self.rope.insert(e.at, &e.new);
            self.changes.push(RowChange { start: start_row, old_end, new_end: start_row + e.new.matches('\n').count() });
        }
        self.sel = txn.after;
        self.version += 1;
        self.undo.push(txn);
        true
    }

    // ------------------------------------------------------------ motions

    pub fn move_to(&mut self, pos: usize, extend: bool) {
        let pos = pos.min(self.len());
        if extend {
            self.sel.head = pos;
        } else {
            self.sel = Sel::at(pos);
        }
    }

    pub fn move_horizontal(&mut self, forward: bool, extend: bool) {
        self.goal_col = None;
        if !extend && !self.sel.is_empty() {
            let r = self.sel.range();
            self.sel = Sel::at(if forward { r.end } else { r.start });
            return;
        }
        let h = self.sel.head;
        let pos = if forward { (h + 1).min(self.len()) } else { h.saturating_sub(1) };
        self.move_to(pos, extend);
    }

    pub fn move_vertical(&mut self, delta: isize, extend: bool) {
        let (row, col) = self.pos_to_rc(self.sel.head);
        let goal = self.goal_col.unwrap_or_else(|| self.display_col(row, col));
        let last = self.line_count().saturating_sub(1) as isize;
        let target = row as isize + delta;
        let pos = if target < 0 {
            0
        } else if target > last {
            self.len()
        } else {
            let r = target as usize;
            self.rc_to_pos(r, self.col_for_display(r, goal))
        };
        if !extend && !self.sel.is_empty() && delta.abs() == 1 {
            // Collapse first, like VS Code, then move from the relevant edge.
            self.sel = Sel::at(self.sel.head);
        }
        self.move_to(pos, extend);
        self.goal_col = Some(goal);
    }

    pub fn word_left_pos(&self, from: usize) -> usize {
        let mut pos = from;
        while pos > 0 && class(self.rope.char(pos - 1)) == CharClass::Space {
            pos -= 1;
        }
        if pos > 0 {
            let cls = class(self.rope.char(pos - 1));
            if cls == CharClass::Newline {
                return pos - 1;
            }
            while pos > 0 && class(self.rope.char(pos - 1)) == cls {
                pos -= 1;
            }
        }
        pos
    }

    pub fn word_right_pos(&self, from: usize) -> usize {
        let len = self.len();
        let mut pos = from;
        while pos < len && class(self.rope.char(pos)) == CharClass::Space {
            pos += 1;
        }
        if pos < len {
            let cls = class(self.rope.char(pos));
            if cls == CharClass::Newline {
                return pos + 1;
            }
            while pos < len && class(self.rope.char(pos)) == cls {
                pos += 1;
            }
        }
        pos
    }

    pub fn move_word(&mut self, forward: bool, extend: bool) {
        self.goal_col = None;
        let pos = if forward { self.word_right_pos(self.sel.head) } else { self.word_left_pos(self.sel.head) };
        self.move_to(pos, extend);
    }

    /// Home: first non-blank char, or column 0 if already there.
    pub fn move_line_start(&mut self, extend: bool) {
        self.goal_col = None;
        let (row, col) = self.pos_to_rc(self.sel.head);
        let indent = self.line(row).chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let target = if col == indent { 0 } else { indent };
        self.move_to(self.line_start(row) + target, extend);
    }

    pub fn move_line_end(&mut self, extend: bool) {
        self.goal_col = None;
        let (row, _) = self.pos_to_rc(self.sel.head);
        self.move_to(self.line_start(row) + self.line_len(row), extend);
    }

    pub fn select_all(&mut self) {
        self.sel = Sel { anchor: 0, head: self.len() };
    }

    pub fn word_range_at(&self, pos: usize) -> Range<usize> {
        let len = self.len();
        let probe = if pos >= len || (pos > 0 && class(self.rope.char(pos)) == CharClass::Newline) {
            pos.saturating_sub(1)
        } else {
            pos
        };
        let Some(ch) = self.char_at(probe) else { return pos..pos };
        let cls = class(ch);
        if cls == CharClass::Newline {
            return pos..pos;
        }
        let mut start = probe;
        while start > 0 && class(self.rope.char(start - 1)) == cls {
            start -= 1;
        }
        let mut end = probe + 1;
        while end < len && class(self.rope.char(end)) == cls {
            end += 1;
        }
        start..end
    }

    /// Range of whole rows `a..=b`, including the trailing newline when there is one.
    pub fn rows_range(&self, a: usize, b: usize) -> Range<usize> {
        let start = self.line_start(a);
        let end = if b + 1 < self.line_count() { self.line_start(b + 1) } else { self.len() };
        start..end
    }

    pub fn select_line(&mut self) {
        let r = self.sel.range();
        let (a, _) = self.pos_to_rc(r.start);
        let (mut b, bcol) = self.pos_to_rc(r.end);
        if !self.sel.is_empty() && bcol == 0 && b > a {
            // Selection already ends at a line start: extend by one more line.
            b = b.min(self.line_count() - 1);
        }
        let range = self.rows_range(a, b);
        self.sel = Sel { anchor: range.start, head: range.end };
    }

    /// The rows covered by the selection (a selection ending at column 0 doesn't include that row).
    pub fn selected_rows(&self) -> (usize, usize) {
        let r = self.sel.range();
        let (a, _) = self.pos_to_rc(r.start);
        let (mut b, bcol) = self.pos_to_rc(r.end);
        if b > a && bcol == 0 {
            b -= 1;
        }
        (a, b)
    }

    // ------------------------------------------------------------ edits

    pub fn insert(&mut self, text: &str) {
        let r = self.sel.range();
        let kind = if text.chars().count() == 1 { EditKind::Insert } else { EditKind::Other };
        self.replace(r, text, None, kind);
    }

    pub fn newline(&mut self) {
        let r = self.sel.range();
        let (row, col) = self.pos_to_rc(r.start);
        let line = self.line(row);
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').take(col).collect();
        let before = self.char_at(r.start.wrapping_sub(1)).filter(|_| r.start > 0);
        let after = self.char_at(r.end);
        let opens = matches!(before, Some('{' | '[' | '('));
        let closes = matches!((before, after), (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')')));
        if opens {
            let inner = format!("{indent}{}", self.indent);
            if closes {
                let text = format!("\n{inner}\n{indent}");
                let cursor = r.start + 1 + inner.chars().count();
                self.replace(r, &text, Some(Sel::at(cursor)), EditKind::Other);
            } else {
                self.replace(r, &format!("\n{inner}"), None, EditKind::Other);
            }
        } else {
            self.replace(r, &format!("\n{indent}"), None, EditKind::Other);
        }
    }

    pub fn newline_below(&mut self) {
        let (row, _) = self.pos_to_rc(self.sel.head);
        let indent: String = self.line(row).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let end = self.line_start(row) + self.line_len(row);
        self.sel = Sel::at(end);
        self.replace(end..end, &format!("\n{indent}"), None, EditKind::Other);
    }

    pub fn newline_above(&mut self) {
        let (row, _) = self.pos_to_rc(self.sel.head);
        let indent: String = self.line(row).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let start = self.line_start(row);
        let cursor = start + indent.chars().count();
        self.replace(start..start, &format!("{indent}\n"), Some(Sel::at(cursor)), EditKind::Other);
    }

    pub fn backspace(&mut self) {
        if !self.sel.is_empty() {
            self.replace(self.sel.range(), "", None, EditKind::Delete);
            return;
        }
        let pos = self.sel.head;
        if pos == 0 {
            return;
        }
        let (row, col) = self.pos_to_rc(pos);
        let line = self.line(row);
        let in_indent = col > 0 && line.chars().take(col).all(|c| c == ' ');
        let unit = self.indent.chars().count();
        let start = if in_indent && !self.indent.starts_with('\t') && unit > 1 {
            let back = match col % unit {
                0 => unit,
                n => n,
            };
            pos - back
        } else {
            // Delete matching pair: "(|)" -> "|"
            let prev = self.rope.char(pos - 1);
            let next = self.char_at(pos);
            if matches!((prev, next), ('(', Some(')')) | ('[', Some(']')) | ('{', Some('}')) | ('"', Some('"')) | ('\'', Some('\'')) | ('`', Some('`'))) {
                self.replace(pos - 1..pos + 1, "", None, EditKind::Delete);
                return;
            }
            pos - 1
        };
        self.replace(start..pos, "", None, EditKind::Delete);
    }

    pub fn delete_forward(&mut self) {
        if !self.sel.is_empty() {
            self.replace(self.sel.range(), "", None, EditKind::Delete);
        } else if self.sel.head < self.len() {
            let p = self.sel.head;
            self.replace(p..p + 1, "", Some(Sel::at(p)), EditKind::Delete);
        }
    }

    pub fn delete_word(&mut self, forward: bool) {
        if !self.sel.is_empty() {
            return self.backspace();
        }
        let p = self.sel.head;
        let range = if forward { p..self.word_right_pos(p) } else { self.word_left_pos(p)..p };
        if !range.is_empty() {
            let start = range.start;
            self.replace(range, "", Some(Sel::at(start)), EditKind::Other);
        }
    }

    pub fn delete_to_line_start(&mut self) {
        if !self.sel.is_empty() {
            return self.backspace();
        }
        let p = self.sel.head;
        let (row, col) = self.pos_to_rc(p);
        let start = if col == 0 { p.saturating_sub(1) } else { self.line_start(row) };
        self.replace(start..p, "", None, EditKind::Other);
    }

    /// ctrl-k: delete to end of line, or join with the next line at EOL.
    pub fn kill_line(&mut self) {
        let p = self.sel.head;
        let (row, _) = self.pos_to_rc(p);
        let eol = self.line_start(row) + self.line_len(row);
        let end = if p == eol { (eol + 1).min(self.len()) } else { eol };
        if end > p {
            self.replace(p..end, "", Some(Sel::at(p)), EditKind::Other);
        }
    }

    pub fn indent_lines(&mut self, outdent: bool) {
        if self.sel.is_empty() && !outdent {
            let (row, col) = self.pos_to_rc(self.sel.head);
            let text = if self.indent.starts_with('\t') {
                "\t".to_string()
            } else {
                let unit = self.indent.len().max(1);
                let d = self.display_col(row, col);
                " ".repeat(unit - d % unit)
            };
            self.replace(self.sel.range(), &text, None, EditKind::Other);
            return;
        }
        let (a, b) = self.selected_rows();
        let unit = self.indent.clone();
        let mut out = Vec::new();
        let mut deltas = Vec::new();
        for row in a..=b {
            let line = self.line(row);
            let new_line = if outdent {
                let strip = if line.starts_with('\t') {
                    1
                } else {
                    line.chars().take(unit.chars().count().max(1)).take_while(|c| *c == ' ').count()
                };
                line.chars().skip(strip).collect::<String>()
            } else if line.is_empty() {
                line.clone()
            } else {
                format!("{unit}{line}")
            };
            deltas.push(new_line.chars().count() as isize - line.chars().count() as isize);
            out.push(new_line);
        }
        let shift = |(row, col): (usize, usize)| -> (usize, usize) {
            if row < a || row > b || col == 0 {
                return (row, col);
            }
            (row, (col as isize + deltas[row - a]).max(0) as usize)
        };
        let anchor = shift(self.pos_to_rc(self.sel.anchor));
        let head = shift(self.pos_to_rc(self.sel.head));
        let range = self.line_start(a)..self.line_start(b) + self.line_len(b);
        self.replace(range, &out.join("\n"), None, EditKind::Other);
        self.sel = Sel { anchor: self.rc_to_pos(anchor.0, anchor.1), head: self.rc_to_pos(head.0, head.1) };
    }

    pub fn move_lines(&mut self, down: bool) {
        let (a, b) = self.selected_rows();
        let last = self.line_count() - 1;
        if (down && b >= last) || (!down && a == 0) {
            return;
        }
        let block: Vec<String> = (a..=b).map(|r| self.line(r)).collect();
        let Sel { anchor, head } = self.sel;
        let (range, text, shift) = if down {
            let next = self.line(b + 1);
            let range = self.line_start(a)..self.line_start(b + 1) + self.line_len(b + 1);
            (range, format!("{next}\n{}", block.join("\n")), next.chars().count() as isize + 1)
        } else {
            let prev = self.line(a - 1);
            let range = self.line_start(a - 1)..self.line_start(b) + self.line_len(b);
            (range, format!("{}\n{prev}", block.join("\n")), -(prev.chars().count() as isize + 1))
        };
        let sel = Sel { anchor: (anchor as isize + shift) as usize, head: (head as isize + shift) as usize };
        self.replace(range, &text, Some(sel), EditKind::Other);
    }

    pub fn duplicate_lines(&mut self, down: bool) {
        let (a, b) = self.selected_rows();
        let block: Vec<String> = (a..=b).map(|r| self.line(r)).collect();
        let text = block.join("\n");
        let end = self.line_start(b) + self.line_len(b);
        let shift = if down { text.chars().count() + 1 } else { 0 };
        let Sel { anchor, head } = self.sel;
        self.replace(end..end, &format!("\n{text}"), Some(Sel { anchor: anchor + shift, head: head + shift }), EditKind::Other);
    }

    pub fn delete_lines(&mut self) {
        let (a, b) = self.selected_rows();
        let mut range = self.rows_range(a, b);
        if range.end == self.len() && a > 0 {
            range.start -= 1; // also remove the preceding newline on the last line
        }
        let row = a.min(self.line_count().saturating_sub(2));
        let col = self.pos_to_rc(self.sel.head).1;
        self.replace(range, "", None, EditKind::Other);
        self.sel = Sel::at(self.rc_to_pos(row, col));
    }

    /// ⌘/ for languages without line comments (HTML, CSS…): wraps each line as `<!-- line -->`.
    pub fn toggle_block_comment(&mut self, open: &str, close: &str) {
        let (a, b) = self.selected_rows();
        let lines: Vec<String> = (a..=b).map(|r| self.line(r)).collect();
        let non_empty: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
        if non_empty.is_empty() {
            return;
        }
        let commented = |l: &str| {
            let t = l.trim();
            t.starts_with(open) && t.ends_with(close) && t.len() >= open.len() + close.len()
        };
        let all_commented = non_empty.iter().all(|l| commented(l));
        let out: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.trim().is_empty() {
                    return l.clone();
                }
                let ws = l.len() - l.trim_start().len();
                let (indent, body) = l.split_at(ws);
                let body = body.trim_end();
                if all_commented {
                    let inner = &body[open.len()..body.len() - close.len()];
                    let inner = inner.strip_prefix(' ').unwrap_or(inner);
                    let inner = inner.strip_suffix(' ').unwrap_or(inner);
                    format!("{indent}{inner}")
                } else {
                    format!("{indent}{open} {body} {close}")
                }
            })
            .collect();
        let had_selection = !self.sel.is_empty();
        let (row, col) = self.pos_to_rc(self.sel.head);
        let delta = out[row - a].chars().count() as isize - lines[row - a].chars().count() as isize;
        let range = self.line_start(a)..self.line_start(b) + self.line_len(b);
        self.replace(range, &out.join("\n"), None, EditKind::Other);
        if had_selection {
            self.sel = Sel { anchor: self.line_start(a), head: self.line_start(b) + self.line_len(b) };
        } else {
            let open_shift = if all_commented { 0 } else { open.chars().count() as isize + 1 };
            let new_col = (col as isize + if col == 0 { 0 } else { open_shift.min(delta.max(-(col as isize))) }).max(0) as usize;
            self.sel = Sel::at(self.rc_to_pos(row, new_col));
        }
    }

    pub fn toggle_comment(&mut self, prefix: &str) {
        let (a, b) = self.selected_rows();
        let lines: Vec<String> = (a..=b).map(|r| self.line(r)).collect();
        let non_empty: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
        if non_empty.is_empty() {
            return;
        }
        let all_commented = non_empty.iter().all(|l| l.trim_start().starts_with(prefix));
        let min_indent = non_empty.iter().map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
        let out: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.trim().is_empty() {
                    return l.clone();
                }
                if all_commented {
                    let ws = l.len() - l.trim_start().len();
                    let rest = &l[ws + prefix.len()..];
                    let rest = rest.strip_prefix(' ').unwrap_or(rest);
                    format!("{}{}", &l[..ws], rest)
                } else {
                    format!("{}{} {}", &l[..min_indent], prefix, &l[min_indent..])
                }
            })
            .collect();
        let range = self.line_start(a)..self.line_start(b) + self.line_len(b);
        let had_selection = !self.sel.is_empty();
        let (row, col) = self.pos_to_rc(self.sel.head);
        let delta = out[row - a].chars().count() as isize - lines[row - a].chars().count() as isize;
        self.replace(range, &out.join("\n"), None, EditKind::Other);
        if had_selection {
            let end_row = b;
            self.sel = Sel { anchor: self.line_start(a), head: self.line_start(end_row) + self.line_len(end_row) };
        } else {
            let new_col = (col as isize + delta).max(0) as usize;
            self.sel = Sel::at(self.rc_to_pos(row, new_col));
        }
    }

    // ------------------------------------------------------------ search

    pub fn find(&self, query: &str, from: usize, forward: bool) -> Option<Range<usize>> {
        if query.is_empty() {
            return None;
        }
        let text = self.text();
        let ci = !query.chars().any(|c| c.is_uppercase());
        let (hay, needle) = if ci { (text.to_lowercase(), query.to_lowercase()) } else { (text.clone(), query.to_string()) };
        if hay.len() != text.len() {
            // Lowercasing changed byte lengths; fall back to case-sensitive search.
            return self.find_in(&text, query, from, forward);
        }
        self.find_in(&hay, &needle, from, forward)
    }

    fn find_in(&self, hay: &str, needle: &str, from: usize, forward: bool) -> Option<Range<usize>> {
        let from_byte = self.rope.char_to_byte(from.min(self.len()));
        let found = if forward {
            hay[from_byte..].find(needle).map(|i| i + from_byte).or_else(|| hay.find(needle))
        } else {
            hay[..from_byte].rfind(needle).or_else(|| hay.rfind(needle))
        }?;
        let start = self.rope.byte_to_char(found);
        let end = self.rope.byte_to_char(found + needle.len());
        Some(start..end)
    }

    pub fn find_all(&self, query: &str, rows: Range<usize>) -> Vec<(usize, Range<usize>)> {
        let mut out = Vec::new();
        if query.is_empty() {
            return out;
        }
        let ci = !query.chars().any(|c| c.is_uppercase());
        let needle = if ci { query.to_lowercase() } else { query.to_string() };
        for row in rows {
            let line = self.line(row);
            let hay = if ci { line.to_lowercase() } else { line.clone() };
            if hay.len() != line.len() {
                continue;
            }
            let mut start = 0;
            while let Some(i) = hay[start..].find(&needle) {
                let b = start + i;
                let c0 = line[..b].chars().count();
                let c1 = c0 + line[b..b + needle.len()].chars().count();
                out.push((row, c0..c1));
                start = b + needle.len().max(1);
            }
        }
        out
    }
}

fn detect_indent(text: &str) -> String {
    let (mut tabs, mut spaces2, mut spaces4) = (0, 0, 0);
    for line in text.lines().take(500) {
        if line.starts_with('\t') {
            tabs += 1;
        } else {
            let n = line.chars().take_while(|c| *c == ' ').count();
            if n > 0 && line.len() > n {
                if n % 4 == 0 {
                    spaces4 += 1;
                }
                if n % 2 == 0 && n % 4 != 0 {
                    spaces2 += 1;
                }
            }
        }
    }
    if tabs > spaces2 + spaces4 {
        "\t".into()
    } else if spaces2 > 0 || spaces4 == 0 {
        "  ".into()
    } else {
        "    ".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_undo_groups_typing() {
        let mut b = Buffer::new("");
        for ch in "hello".chars() {
            b.insert(&ch.to_string());
        }
        assert_eq!(b.text(), "hello");
        b.undo();
        assert_eq!(b.text(), "");
        b.redo();
        assert_eq!(b.text(), "hello");
    }

    #[test]
    fn newline_keeps_indent_and_splits_braces() {
        let mut b = Buffer::new("  if (x) {}");
        b.sel = Sel::at(10);
        b.newline();
        assert_eq!(b.text(), "  if (x) {\n    \n  }");
        assert_eq!(b.pos_to_rc(b.sel.head), (1, 4));
    }

    #[test]
    fn word_motion() {
        let b = Buffer::new("foo.bar  baz");
        assert_eq!(b.word_right_pos(0), 3);
        assert_eq!(b.word_right_pos(3), 4);
        assert_eq!(b.word_right_pos(7), 12);
        assert_eq!(b.word_left_pos(12), 9);
    }

    #[test]
    fn vertical_motion_keeps_goal_column() {
        let mut b = Buffer::new("abcdef\nab\nabcdef");
        b.sel = Sel::at(5);
        b.move_vertical(1, false);
        assert_eq!(b.pos_to_rc(b.sel.head), (1, 2));
        b.move_vertical(1, false);
        assert_eq!(b.pos_to_rc(b.sel.head), (2, 5));
    }

    #[test]
    fn move_and_duplicate_lines() {
        let mut b = Buffer::new("a\nb\nc");
        b.sel = Sel::at(0);
        b.move_lines(true);
        assert_eq!(b.text(), "b\na\nc");
        assert_eq!(b.pos_to_rc(b.sel.head), (1, 0));
        b.duplicate_lines(true);
        assert_eq!(b.text(), "b\na\na\nc");
        assert_eq!(b.pos_to_rc(b.sel.head), (2, 0));
    }

    #[test]
    fn toggle_comment_round_trips() {
        let mut b = Buffer::new("  let a = 1;\n  let b = 2;");
        b.select_all();
        b.toggle_comment("//");
        assert_eq!(b.text(), "  // let a = 1;\n  // let b = 2;");
        b.toggle_comment("//");
        assert_eq!(b.text(), "  let a = 1;\n  let b = 2;");
    }

    #[test]
    fn block_comment_round_trips() {
        let mut b = Buffer::new("  <div>hi</div>\n  <p/>");
        b.select_all();
        b.toggle_block_comment("<!--", "-->");
        assert_eq!(b.text(), "  <!-- <div>hi</div> -->\n  <!-- <p/> -->");
        b.toggle_block_comment("<!--", "-->");
        assert_eq!(b.text(), "  <div>hi</div>\n  <p/>");
    }

    #[test]
    fn tabs_expand_for_display() {
        let b = Buffer::new("\tx");
        let (s, map) = b.display_line(0);
        assert_eq!(s, "    x");
        assert_eq!(map, vec![0, 4, 5]);
        assert_eq!(b.col_for_display(0, 4), 1);
    }

    #[test]
    fn soft_tab_backspace() {
        let mut b = Buffer::new("    x");
        b.sel = Sel::at(4);
        b.indent = "  ".into();
        b.backspace();
        assert_eq!(b.text(), "  x");
    }

    #[test]
    fn find_wraps_around() {
        let b = Buffer::new("one two one");
        assert_eq!(b.find("one", 1, true), Some(8..11));
        assert_eq!(b.find("one", 9, true), Some(0..3));
        assert_eq!(b.find("ONE", 0, true), None);
    }
}
