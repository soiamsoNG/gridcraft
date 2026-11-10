//! The in-cell editor covers the edited cell exactly, as in Excel, at every zoom (#1755).

use egui::{Color32, Event, Id, Modifiers, PointerButton, Rect, Shape};
use gridcraft_engine::Session;
use gridcraft_engine::core::CellRef;
use gridcraft_ui_egui::{SheetApp, grid::Geo};
use serde_json::json;

type Harness = egui_kittest::Harness<'static, SheetApp>;

fn harness() -> Harness {
    let mut session = Session::new();
    session.new_workbook();
    let mut h = crate::common::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut SheetApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        crate::common::app(session),
    );
    h.run_steps(4);
    h
}

fn cell_rect(h: &Harness, cell: &str) -> Rect {
    let app = h.state();
    let sheet = app.session.active().unwrap().wb.active().unwrap();
    let geo = Geo::new(sheet, app.grid.rect.unwrap(), app.view().scroll);
    geo.cell_rect(sheet, CellRef::parse(cell).unwrap())
}

fn click_cell(h: &mut Harness, cell: &str) {
    let pos = cell_rect(h, cell).center();
    h.input_mut().events.extend([
        Event::PointerMoved(pos),
        Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
        Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
    ]);
    h.run_steps(2);
}

fn type_text(h: &mut Harness, text: &str) {
    h.input_mut().events.push(Event::Text(text.into()));
    h.run_steps(3);
}

/// The white editor background painted from the cell's top-left corner.
fn editor_rect(h: &Harness, cell: Rect) -> Rect {
    h.output()
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            Shape::Rect(r) if r.fill == Color32::WHITE && (r.rect.min - cell.min).length() < 0.01 => Some(r.rect),
            _ => None,
        })
        .next_back()
        .expect("in-cell editor background")
}

fn assert_close(a: Rect, b: Rect, what: &str) {
    assert!((a.min - b.min).length() < 0.01 && (a.max - b.max).length() < 0.01, "{what}: editor {a:?} must match cell {b:?}");
}

#[test]
fn editor_matches_the_cell_at_every_zoom() {
    for zoom in [50u16, 100, 200, 400] {
        let mut h = harness();
        h.state_mut().run("view.zoom", json!({"percent": zoom})).unwrap();
        h.run_steps(3);
        click_cell(&mut h, "B2");
        type_text(&mut h, "abc");
        assert!(h.state().editor.is_some());
        let cell = cell_rect(&h, "B2");
        assert_close(editor_rect(&h, cell), cell, &format!("zoom {zoom}"));
    }
}

#[test]
fn one_long_line_widens_the_editor_but_keeps_the_row_height() {
    let mut h = harness();
    click_cell(&mut h, "B2");
    type_text(&mut h, "a much longer line of text than the cell can hold");
    let cell = cell_rect(&h, "B2");
    let ed = editor_rect(&h, cell);
    assert!(ed.width() > cell.width() + 1.0, "long text widens the editor: {ed:?} vs {cell:?}");
    assert!((ed.height() - cell.height()).abs() < 0.01, "one line keeps the row height: {ed:?} vs {cell:?}");
}

/// The editing text sits where the cell's own text sits: it follows the cell's vertical alignment,
/// so entering or leaving the edit does not move it.
#[test]
fn the_editor_text_follows_the_cells_vertical_alignment() {
    let mut tall = Vec::new();
    let mut top_inks = Vec::new();
    for (align, command) in [("top", Some("home.alignTop")), ("center", Some("home.alignMiddle")), ("bottom", None)] {
        for height in [20.0, 200.0] {
            let mut h = harness();
            h.state_mut().session.execute("home.rowHeight", json!({"rows": "1:1", "height": height})).unwrap();
            if let Some(c) = command {
                h.state_mut().session.execute(c, json!({"range": "A1"})).unwrap();
            }
            h.run_steps(2);
            h.state_mut().begin_edit(Some("Hello 中文".into()), false);
            h.run_steps(2);
            let field = h.ctx.read_response(Id::unique("gridcraft.cell_editor")).expect("the in-cell editor is shown").rect;
            let cell = cell_rect(&h, "A1");
            let ink_top: f32 = h.ctx.data(|d| d.get_temp(Id::unique("gridcraft.cell_editor.ink"))).expect("the editor recorded its ink gap");
            let gap = field.top() + ink_top - cell.top();
            if height > 100.0 {
                tall.push(gap);
            }
            // Top alignment is the one that ignores the row height: the ink starts 2 z below the edge.
            if align == "top" {
                top_inks.push(gap);
            }
        }
    }
    assert!((top_inks[0] - top_inks[1]).abs() < 0.51, "top: the ink moved with the row height: {top_inks:?}");
    assert!(tall[0] < tall[1] && tall[1] < tall[2], "top < center < bottom in a tall row: {tall:?}");
    assert!(tall[2] - tall[0] > 100.0, "a tall row separates the three alignments: {tall:?}");
}

/// An empty cell has no text to measure, so the editor used to lay out zero rows and anchor its
/// field at the cell's bottom edge — the caret then painted in the row below (#1231).
#[test]
fn an_empty_cell_edits_inside_its_own_row() {
    for command in [Some("home.alignTop"), Some("home.alignMiddle"), None] {
        for height in [20.0, 60.0, 200.0] {
            let mut h = harness();
            h.state_mut().session.execute("home.rowHeight", json!({"rows": "1:1", "height": height})).unwrap();
            if let Some(c) = command {
                h.state_mut().session.execute(c, json!({"range": "A1"})).unwrap();
            }
            h.run_steps(2);
            h.state_mut().begin_edit(Some(String::new()), false);
            h.run_steps(2);
            let field = h.ctx.read_response(Id::unique("gridcraft.cell_editor")).expect("the in-cell editor is shown").rect;
            let cell = cell_rect(&h, "A1");
            let what = format!("{command:?} at {height} pt");
            // The bug was a field whose top sat exactly on the cell's bottom edge (zero-row galley).
            assert!(field.top() <= cell.bottom() - 1.0, "{what}: the field starts on or below the cell's bottom edge: field {field:?} cell {cell:?}");
            // A row shorter than its line may let a one-line field overhang, but a tall one must not.
            if height > 100.0 {
                assert!(field.top() >= cell.top() - 0.01, "{what}: the field starts above a tall cell: field {field:?} cell {cell:?}");
            }
        }
    }
}

/// While the in-cell editor is open the grid must not paint the cell's stored value: the editor
/// only covers its own width, so a longer value's overflow into the empty cells on its right used
/// to stay on screen as a ghost once characters were deleted (#1231).
#[test]
fn the_edited_cells_value_is_not_painted_under_the_editor() {
    let painted = |h: &Harness, needle: &str| {
        h.output().shapes.iter().any(|s| match &s.shape {
            Shape::Text(t) => t.galley.job.text.contains(needle),
            _ => false,
        })
    };
    let mut h = harness();
    h.state_mut().run("cell.set", json!({"cell": "A1", "input": "我们🈶笑脸，我们有😀"})).unwrap();
    h.run_steps(2);
    assert!(painted(&h, "笑脸"), "the value paints before editing");
    h.state_mut().begin_edit(Some(String::new()), false);
    h.run_steps(2);
    assert!(h.state().editor.is_some(), "the in-cell editor is open");
    assert!(!painted(&h, "笑脸"), "the stored value must not paint under the in-cell editor");
    h.state_mut().cancel_edit();
    h.run_steps(2);
    h.state_mut().begin_edit(Some(String::new()), true);
    h.run_steps(2);
    assert!(painted(&h, "笑脸"), "editing from the formula bar keeps the cell's value on screen");
}

/// `TextFormat::italics` shears the top of each glyph a quarter of its ink height to the right but
/// leaves the advance alone, so the last letter of an italic line sticks out past the text's width.
/// The editor has to keep that room free on the right, or the sheared top is cut by a vertical edge
/// — most visible with Hangul, whose ink boxes are the tallest (#1231).
#[test]
fn the_editor_leaves_room_for_the_italic_shear() {
    let mut h = harness();
    h.state_mut().session.execute("cell.set", json!({"cell": "A1", "input": "한국어 Hello"})).unwrap();
    h.run_steps(2);
    h.state_mut().begin_edit(Some("한국어 Hello".into()), false);
    h.run_steps(2);
    let painted = editor_rect(&h, cell_rect(&h, "A1"));
    let overhang: f32 = h.ctx.data(|d| d.get_temp(Id::unique("gridcraft.cell_editor.overhang"))).expect("the editor measured its shear");
    let room: f32 = h.ctx.data(|d| d.get_temp(Id::unique("gridcraft.cell_editor.room"))).expect("the editor kept room for it");
    assert_eq!(overhang, 0.0, "upright text does not shear");
    assert!((room - (painted.width() - 4.0)).abs() < 0.51, "upright text uses the whole field: room {room}, field {painted:?}");

    h.state_mut().session.execute("home.italic", json!({"range": "A1", "on": true})).unwrap();
    h.run_steps(2);
    h.state_mut().begin_edit(Some("한국어 Hello".into()), false);
    h.run_steps(2);
    let painted = editor_rect(&h, cell_rect(&h, "A1"));
    let overhang: f32 = h.ctx.data(|d| d.get_temp(Id::unique("gridcraft.cell_editor.overhang"))).expect("the editor measured its shear");
    let room: f32 = h.ctx.data(|d| d.get_temp(Id::unique("gridcraft.cell_editor.room"))).expect("the editor kept room for it");
    assert!(overhang > 0.5, "an italic line shears its glyphs: {overhang}");
    assert!((room - (painted.width() - 4.0)).abs() < 0.51, "the field keeps its own width: room {room}, field {painted:?}");
    // The sheared tops are drawn past the last advance; the field itself is the cell plus, once
    // the text overflows, the slack and the overhang the caller asked for.
    assert!(painted.width() >= cell_rect(&h, "A1").width() - 0.51, "the field is at least the cell: {:?}", painted);
}

#[test]
fn a_long_one_line_text_keeps_the_editor_one_line_tall() {
    let mut h = harness();
    let long = "日本語 한국어 긴문장 that keeps going past the cell";
    h.state_mut().session.execute("cell.set", json!({"cell": "A1", "input": long})).unwrap();
    h.run_steps(2);
    h.state_mut().begin_edit(Some(long.into()), false);
    h.run_steps(2);
    let cell = cell_rect(&h, "A1");
    let field = editor_rect(&h, cell);
    let line: f32 = h.ctx.data(|d| d.get_temp(Id::unique("gridcraft.cell_editor.height"))).expect("the editor measured its line");
    // The probe must be laid out unwrapped: a wrapped probe reports several lines, and the field
    // would then be positioned as if the text were that tall.
    assert!(line < field.height() * 1.5 + 0.51, "one line of text: line {line}, field {:?}", field);
    assert!(field.top() >= cell.top() - 0.51, "the field starts inside the cell: {:?} vs {:?}", field, cell);
}
