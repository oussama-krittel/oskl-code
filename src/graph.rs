//! Lays out commit history into lanes, like VS Code's Source Control graph.
//!
//! Each row is drawn in two halves: the top half connects the previous row
//! into this commit's node, the bottom half connects the node to its parents.

use crate::git::Commit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub from: usize,
    pub to: usize,
    pub color: usize,
}

#[derive(Clone, Debug, Default)]
pub struct GraphRow {
    pub lane: usize,
    pub color: usize,
    pub top: Vec<Segment>,
    pub bottom: Vec<Segment>,
    pub width: usize,
}

#[derive(Clone)]
struct Lane {
    sha: String,
    color: usize,
}

pub fn layout(commits: &[Commit], palette_len: usize) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<Lane>> = Vec::new();
    // Color 0 is reserved for the first-parent chain of the first commit (HEAD side).
    let mut next_color = 1;
    let mut new_color = |next: &mut usize| {
        let c = *next;
        *next = if *next + 1 >= palette_len { 1 } else { *next + 1 };
        c
    };
    let mut rows = Vec::with_capacity(commits.len());

    for (ix, commit) in commits.iter().enumerate() {
        let mut row = GraphRow::default();

        // Find (or create) the lane this commit lives in.
        let existing = lanes.iter().position(|l| l.as_ref().is_some_and(|l| l.sha == commit.sha));
        let (lane, color) = match existing {
            Some(i) => (i, lanes[i].as_ref().unwrap().color),
            None => {
                let color = if ix == 0 { 0 } else { new_color(&mut next_color) };
                let i = free_slot(&mut lanes);
                (i, color)
            }
        };
        row.lane = lane;
        row.color = color;

        // Top half: everything flowing in from the previous row.
        for (i, slot) in lanes.iter().enumerate() {
            if let Some(l) = slot {
                let to = if l.sha == commit.sha { lane } else { i };
                row.top.push(Segment { from: i, to, color: l.color });
            }
        }
        for slot in lanes.iter_mut() {
            if slot.as_ref().is_some_and(|l| l.sha == commit.sha) {
                *slot = None;
            }
        }

        // Bottom half: lanes passing through, then edges to parents.
        for (i, slot) in lanes.iter().enumerate() {
            if let Some(l) = slot {
                row.bottom.push(Segment { from: i, to: i, color: l.color });
            }
        }
        for (pi, parent) in commit.parents.iter().enumerate() {
            if pi == 0 {
                // The first parent always continues straight down; if another lane
                // already waits for it, the two converge at the parent's row.
                lanes_set(&mut lanes, lane, Lane { sha: parent.clone(), color });
                row.bottom.push(Segment { from: lane, to: lane, color });
            } else if let Some(j) = lanes.iter().position(|l| l.as_ref().is_some_and(|l| &l.sha == parent)) {
                let c = lanes[j].as_ref().unwrap().color;
                row.bottom.push(Segment { from: lane, to: j, color: c });
            } else {
                let c = new_color(&mut next_color);
                let j = free_slot_after(&mut lanes, lane);
                lanes[j] = Some(Lane { sha: parent.clone(), color: c });
                row.bottom.push(Segment { from: lane, to: j, color: c });
            }
        }

        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }
        let used = |s: &Segment| s.from.max(s.to) + 1;
        row.width = row
            .top
            .iter()
            .chain(row.bottom.iter())
            .map(used)
            .max()
            .unwrap_or(0)
            .max(lane + 1);
        rows.push(row);
    }
    rows
}

fn lanes_set(lanes: &mut Vec<Option<Lane>>, i: usize, lane: Lane) {
    if i >= lanes.len() {
        lanes.resize(i + 1, None);
    }
    lanes[i] = Some(lane);
}

fn free_slot(lanes: &mut Vec<Option<Lane>>) -> usize {
    match lanes.iter().position(|l| l.is_none()) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

fn free_slot_after(lanes: &mut Vec<Option<Lane>>, after: usize) -> usize {
    match lanes.iter().enumerate().position(|(i, l)| i > after && l.is_none()) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sha: &str, parents: &[&str]) -> Commit {
        Commit {
            sha: sha.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: String::new(),
            email: String::new(),
            time: 0,
            refs: vec![],
            subject: String::new(),
        }
    }

    #[test]
    fn linear_history_uses_one_lane() {
        let rows = layout(&[c("c", &["b"]), c("b", &["a"]), c("a", &[])], 6);
        assert!(rows.iter().all(|r| r.lane == 0 && r.width == 1));
    }

    #[test]
    fn merge_opens_and_closes_a_lane() {
        // m merges f into b; f branches off a.
        let rows = layout(&[c("m", &["b", "f"]), c("f", &["a"]), c("b", &["a"]), c("a", &[])], 6);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[1].lane, 1);
        assert_eq!(rows[2].lane, 0);
        assert_eq!(rows[3].lane, 0);
        // Lane 1 converges back into lane 0 at `a`.
        assert!(rows[3].top.iter().any(|s| s.from == 1 && s.to == 0));
    }
}
