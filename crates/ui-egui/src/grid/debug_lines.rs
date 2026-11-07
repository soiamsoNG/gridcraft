//! Temporary probes that show where a cell's text sits (#1231).
//!
//! Two paths draw them: `quadrant.rs` for a cell at rest, `editor.rs` for the cell being edited.
//! Both go through [`draw`], so a screenshot reads the same way whichever path drew it. The
//! palette, the dash pattern and the width each line covers are documented in
//! `docs/font-flow-linux-rich-text.md` (§9, "Debug line colours").
//!
//! Switched on with `GRIDCRAFT_DEBUG_LINES=baseline,boxes,ink` (any of the three, or `all`).
//! `GRIDCRAFT_BASELINE` and `GRIDCRAFT_EDITOR_DEBUG` still switch everything on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

use egui::epaint::Shape;
use egui::{Color32, Painter, Pos2, Stroke};

use crate::script::{ScriptClass, classify};

/// One line in the probe palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Line {
    /// Where the text of a row sits after `shared_baseline()`.
    SharedBaseline,
    /// Where each script's glyphs sat before `shared_baseline()`, one span per script.
    RawBaseline,
    /// The cell rectangle handed to the paint code.
    CellRect,
    /// The rectangle handed to the cell editor's `TextEdit`.
    FieldRect,
    /// The line box `TextEdit` uses for the caret and the selection.
    RowBox,
    /// The ink of a row, top and bottom.
    InkBounds,
}

impl Line {
    fn color(self) -> Color32 {
        match self {
            Self::SharedBaseline => Color32::from_rgb(255, 0, 0),
            Self::RawBaseline => Color32::from_rgb(0, 255, 0),
            Self::CellRect => Color32::from_rgb(255, 235, 0),
            Self::FieldRect => Color32::from_rgb(0, 200, 255),
            Self::RowBox => Color32::from_rgb(255, 0, 200),
            Self::InkBounds => Color32::from_rgb(128, 0, 255),
        }
    }

    /// Dash and gap, in pixels. Every line is dashed with a pair of primes, so two lines that land
    /// on the same y keep different periods: they interleave instead of one hiding the other. A
    /// probe that only showed where the correction worked would hide exactly the case it exists to
    /// catch — a script that should have moved and did not.
    fn dashes(self) -> (f32, f32) {
        match self {
            Self::SharedBaseline => (2.0, 3.0),
            Self::RawBaseline => (3.0, 5.0),
            Self::CellRect => (5.0, 7.0),
            Self::FieldRect => (7.0, 11.0),
            Self::RowBox => (11.0, 13.0),
            Self::InkBounds => (13.0, 17.0),
        }
    }

    /// Paint order, low first. Baselines sit under the boxes, and the shared baseline sits over the
    /// raw one. `paint_*` sorts by this, so the order does not depend on which helper runs first.
    fn order(self) -> u8 {
        match self {
            Self::InkBounds => 1,
            Self::RawBaseline => 2,
            Self::SharedBaseline => 3,
            Self::CellRect => 4,
            Self::FieldRect => 5,
            Self::RowBox => 6,
        }
    }

    /// Whether this line is drawn with no editor open, while editing, or both.
    fn shows_in(self, scope: Scope) -> bool {
        match self {
            Self::FieldRect | Self::RowBox => scope == Scope::Edit,
            Self::SharedBaseline | Self::RawBaseline | Self::CellRect | Self::InkBounds => true,
        }
    }

    fn group(self) -> Group {
        match self {
            Self::SharedBaseline | Self::RawBaseline => Group::Baseline,
            Self::CellRect | Self::FieldRect | Self::RowBox => Group::Boxes,
            Self::InkBounds => Group::Ink,
        }
    }
}

/// Which of the two drawing paths a probe line belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// The sheet as it looks with no editor open.
    Display,
    /// The cell being edited.
    Edit,
}

/// The three switches `GRIDCRAFT_DEBUG_LINES` accepts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Group {
    Baseline,
    Boxes,
    Ink,
}

fn groups() -> &'static [Group] {
    static GROUPS: OnceLock<Vec<Group>> = OnceLock::new();
    GROUPS
        .get_or_init(|| {
            if ["GRIDCRAFT_BASELINE", "GRIDCRAFT_EDITOR_DEBUG"].iter().any(|name| std::env::var_os(name).is_some()) {
                return vec![Group::Baseline, Group::Boxes, Group::Ink];
            }
            let Ok(raw) = std::env::var("GRIDCRAFT_DEBUG_LINES") else { return Vec::new() };
            let mut groups = Vec::new();
            for part in raw.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
                match part {
                    "all" => return vec![Group::Baseline, Group::Boxes, Group::Ink],
                    "baseline" | "baselines" => groups.push(Group::Baseline),
                    "boxes" | "box" => groups.push(Group::Boxes),
                    "ink" => groups.push(Group::Ink),
                    other => log::warn!("GRIDCRAFT_DEBUG_LINES: unknown group {other:?}"),
                }
            }
            groups
        })
        .as_slice()
}

/// Whether any probe is switched on.
pub(crate) fn any() -> bool {
    !groups().is_empty()
}

/// Whether the probe for `group` is switched on.
pub(crate) fn enabled(group: Group) -> bool {
    groups().contains(&group)
}

/// One probe line waiting to be drawn: which line, its y, and the span of x it covers.
type Probe = (Line, f32, (f32, f32));

/// Draws one horizontal probe line across `x`.
fn draw(painter: &Painter, line: Line, y: f32, x: (f32, f32)) {
    if x.0 >= x.1 || !y.is_finite() {
        return;
    }
    let stroke = Stroke::new(1.0, line.color());
    let ends = [Pos2::new(x.0, y), Pos2::new(x.1, y)];
    let (dash, gap) = line.dashes();
    painter.extend(Shape::dashed_line(&ends, stroke, dash, gap));
}

/// Draws every collected probe in [`Line::order`] order, skipping the groups that are switched off.
fn paint_probes(painter: &Painter, mut probes: Vec<Probe>, scope: Scope) {
    probes.sort_by_key(|(line, _, _)| line.order());
    for (line, y, x) in probes {
        if line.shows_in(scope) && enabled(line.group()) {
            draw(painter, line, y, x);
        }
    }
}

// How far each glyph of a row sat from the row's first glyph before `shared_baseline()` moved it,
// keyed by the address of the galley that came out of it. Offsets rather than positions keep the
// green line on the text when a later step shifts the whole row uniformly (`normalize_row_boxes`).
// Only filled while a probe is on.
thread_local! {
    static RAW: RefCell<HashMap<usize, Vec<Vec<f32>>>> = RefCell::new(HashMap::new());
}

const RAW_LIMIT: usize = 4096;

fn key(galley: &Arc<egui::Galley>) -> usize {
    Arc::as_ptr(galley) as usize
}

/// Remembers, per row, how far each glyph sat from that row's first glyph before the correction.
pub(crate) fn remember_raw(galley: &Arc<egui::Galley>, rows: Vec<Vec<f32>>) {
    if !enabled(Group::Baseline) {
        return;
    }
    RAW.with(|raw| {
        let mut raw = raw.borrow_mut();
        if raw.len() >= RAW_LIMIT {
            raw.clear();
        }
        raw.insert(key(galley), rows);
    });
}

fn raw_rows(galley: &Arc<egui::Galley>) -> Option<Vec<Vec<f32>>> {
    RAW.with(|raw| raw.borrow().get(&key(galley)).cloned())
}

/// The horizontal extent of the glyphs `[start, end)` of a row, from the left edge of the first
/// glyph's ink to the right edge of the last one's.
fn ink_span(glyphs: &[egui::epaint::text::Glyph]) -> Option<(f32, f32)> {
    let mut left = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    for glyph in glyphs {
        left = left.min(glyph.pos.x + glyph.uv_rect.offset.x);
        right = right.max(glyph.pos.x + glyph.uv_rect.offset.x + glyph.uv_rect.size.x);
    }
    (left.is_finite() && right.is_finite() && left < right).then_some((left, right))
}

/// Splits a row into runs of one script, as `crate::script::classify` sees the glyphs. `Common`
/// joins the run beside it, so the space between two scripts does not break a span.
fn script_runs(glyphs: &[egui::epaint::text::Glyph]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize, ScriptClass)> = Vec::new();
    for (i, glyph) in glyphs.iter().enumerate() {
        let script = classify(glyph.chr);
        match runs.last_mut() {
            Some((_, end, run_script)) => {
                if script == ScriptClass::Common || *run_script == ScriptClass::Common || *run_script == script {
                    *end = i + 1;
                    if *run_script == ScriptClass::Common {
                        *run_script = script;
                    }
                } else {
                    runs.push((i, i + 1, script));
                }
            }
            None => runs.push((i, i + 1, script)),
        }
    }
    runs.into_iter().map(|(start, end, _)| (start, end)).collect()
}

/// Collects the probe lines of a laid-out galley whose top-left corner sits at `origin`.
fn collect_galley(probes: &mut Vec<Probe>, galley: &Arc<egui::Galley>, origin: Pos2) {
    let raw = raw_rows(galley);
    for (r, placed) in galley.rows.iter().enumerate() {
        let glyphs = &placed.row.glyphs;
        let Some((left, right)) = ink_span(glyphs) else { continue };
        let x = (origin.x + left, origin.x + right);
        let y = origin.y + placed.pos.y;
        let Some(first) = glyphs.first() else { continue };
        let shared = first.pos.y;
        if enabled(Group::Baseline)
            && let Some(row_raw) = raw.as_ref().and_then(|rows| rows.get(r))
        {
            // Every script run gets its line, including the ones already on the shared baseline:
            // there the two lines interleave (their dash periods are coprime), which says "this run
            // was not moved" instead of saying nothing at all.
            for (start, end) in script_runs(glyphs) {
                let Some(span) = ink_span(&glyphs[start..end]) else { continue };
                let offset = row_raw.get(start).copied().unwrap_or(0.0);
                probes.push((Line::RawBaseline, y + shared + offset, (origin.x + span.0, origin.x + span.1)));
            }
        }
        probes.push((Line::SharedBaseline, y + shared, x));
        if enabled(Group::Ink) {
            let mut top = f32::INFINITY;
            let mut bottom = f32::NEG_INFINITY;
            for glyph in glyphs {
                let glyph_top = glyph.pos.y + glyph.uv_rect.offset.y;
                top = top.min(glyph_top);
                bottom = bottom.max(glyph_top + glyph.uv_rect.size.y);
            }
            if top.is_finite() && bottom.is_finite() {
                probes.push((Line::InkBounds, y + top, x));
                probes.push((Line::InkBounds, y + bottom, x));
            }
        }
    }
}

/// Probes for a cell at rest: its rectangle plus the baseline and ink lines of its galley.
/// `active` limits the cell outline to the selected cell: every other outline would land on a
/// grid line and tell nothing.
pub(crate) fn paint_cell(painter: &Painter, rect: egui::Rect, galley: &Arc<egui::Galley>, origin: Pos2, active: bool) {
    if groups().is_empty() {
        return;
    }
    let mut probes = Vec::new();
    if active && enabled(Group::Boxes) {
        probes.push((Line::CellRect, rect.top(), (rect.left(), rect.right())));
        probes.push((Line::CellRect, rect.bottom(), (rect.left(), rect.right())));
    }
    collect_galley(&mut probes, galley, origin);
    paint_probes(painter, probes, Scope::Display);
}

/// Probes for the cell editor: its cell, its input field, its line box, and the same text lines.
pub(crate) fn paint_editor(painter: &Painter, cell: egui::Rect, field: egui::Rect, galley: &Arc<egui::Galley>, text_top: f32, gh: f32) {
    if groups().is_empty() {
        return;
    }
    let mut probes = Vec::new();
    if enabled(Group::Boxes) {
        probes.push((Line::CellRect, cell.top(), (cell.left(), cell.right())));
        probes.push((Line::CellRect, cell.bottom(), (cell.left(), cell.right())));
        probes.push((Line::FieldRect, field.top(), (field.left(), field.right())));
        probes.push((Line::FieldRect, field.bottom(), (field.left(), field.right())));
        probes.push((Line::RowBox, field.top() + gh, (field.left(), field.right())));
    }
    collect_galley(&mut probes, galley, Pos2::new(field.left(), text_top));
    paint_probes(painter, probes, Scope::Edit);
}
