use super::{SheetApp, theme};
use crate::fonts;

/// Stop the script scan after this many characters when deciding which fallback faces to load.
const SCAN_LIMIT: usize = 200_000;

impl SheetApp {
    /// Test-harness setup: mark a deterministic font set (see
    /// [`theme::pinned_font_definitions`]) as already installed, so the first `logic` pass neither
    /// starts the host font scan nor replaces the harness's pinned set. Production uses
    /// [`Self::setup_context`] instead.
    #[doc(hidden)]
    pub fn use_prepared_fonts(&mut self) {
        self.prepared_fonts = true;
        self.fonts_ready = true;
        self.fonts_set = true;
        self.fonts_rebind = false;
        self.fonts_han = Some(theme::HanOrder::of(self.ui.language));
        self.fonts_bound = self.used_fonts();
        self.fonts_scripts = self.used_scripts();
        self.fonts_seen = self.fonts_key();
    }

    /// The font names the open workbooks use: every style's font plus the theme's heading/body
    /// fonts. These are the families [`fonts`] binds an installed file for. Every workbook's, as
    /// each may be showing in a window of its own.
    pub fn used_fonts(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for d in self.session.documents() {
            names.extend(d.wb.styles.iter().map(|(_, s)| s.font.name.clone()));
            names.push(d.wb.theme.major_font.clone());
            names.push(d.wb.theme.minor_font.clone());
            // Text boxes and shapes name their own typefaces.
            names.extend(d.wb.sheets.iter().flat_map(|sh| sh.shapes.iter().filter_map(|s| s.format.font.clone())));
        }
        // One name per family: every style entry repeats the workbook fonts, so a bold toggle that
        // adds a style must not look like a new font set and rebuild the atlas (#1231).
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The scripts the open workbooks' text is written in. The font set loads a fallback face only
    /// for a script that is actually present, instead of every face the host happens to have.
    pub fn used_scripts(&self) -> crate::script::ScriptSet {
        self.scripts_in(false)
    }

    /// The scripts the open workbooks set in **bold**, for the bold fallback chains.
    pub fn used_bold_scripts(&self) -> crate::script::ScriptSet {
        self.scripts_in(true)
    }

    /// The scripts of the text in the active workbook; `bold` keeps only the cells whose style is
    /// bold. Shapes carry their own text, so a text box in Thai counts like a cell in Thai.
    ///
    /// The scan stops after [`SCAN_LIMIT`] characters: the scripts first seen say what to load, and a
    /// hostile sheet of millions of cells must not stall the frame.
    fn scripts_in(&self, bold: bool) -> crate::script::ScriptSet {
        let mut scripts = crate::script::ScriptSet::EMPTY;
        let mut seen = 0usize;
        let Some(d) = self.session.active() else { return scripts };
        let texts = d.wb.sheets.iter().flat_map(|sheet| {
            let shapes = sheet.shapes.iter().map(|shape| shape.text.as_str());
            let cells = sheet.cells.iter().filter_map(move |(_, cell)| {
                if bold && !d.wb.styles.get(cell.style).font.bold {
                    return None;
                }
                match &cell.value {
                    gridcraft_engine::core::Value::Text(text) => Some(&**text),
                    _ => None,
                }
            });
            shapes.chain(cells)
        });
        for text in texts {
            scripts = scripts.union(crate::script::ScriptSet::of_text(text));
            seen += text.chars().count();
            if seen >= SCAN_LIMIT {
                break;
            }
        }
        scripts
    }

    /// The open workbooks' (uid, revision), folded: [`Self::used_fonts`] changes only with it.
    fn fonts_key(&self) -> (u64, u64) {
        self.session
            .documents()
            .iter()
            .fold((0u64, 0u64), |(uid, rev), d| (uid.wrapping_mul(31).wrapping_add(d.uid), rev.wrapping_mul(31).wrapping_add(d.revision)))
    }

    /// Installs the text-width closure once, on the first frame.
    pub(super) fn ensure_text_width(&mut self, ctx: &egui::Context) {
        if self.session.text_width.is_some() {
            return;
        }
        let ctx = ctx.clone();
        self.session.text_width = Some(std::sync::Arc::new(move |text, font, exact| theme::cell_text_width(&ctx, text, font, exact)));
    }

    /// Applies the font gates: a language Han-order change, startup/prepared fonts and workbook
    /// font binding. Every branch keeps its one-frame paint gate.
    pub(super) fn apply_font_state(&mut self, ctx: &egui::Context) {
        // A pending synthetic step owns the next real `ui` pass: `ui` returns at the font gate
        // before it handles input, so a font change requested now would consume the step without
        // applying it. Delay the change one frame; the hidden-paint request keeps it coming.
        let defer_font_change = self.synthetic_pending || !self.synthetic.is_empty();
        if !defer_font_change && self.fonts_han.is_some_and(|han| han != theme::HanOrder::of(self.ui.language)) {
            self.fonts_ready = false;
            self.fonts_set = false;
        }
        if !self.fonts_ready {
            self.start_font_setup(ctx);
        } else if self.fonts_rebind {
            // A font-set change made last frame is active now: publish its roles.
            self.fonts_rebind = false;
            let roles = std::mem::take(&mut self.pending_roles);
            if let Some(defs) = &self.fonts_defs {
                crate::fonts::set_bound_for(defs, roles);
            }
        } else if !defer_font_change && !self.prepared_fonts && fonts::scan_done() {
            self.rebind_workbook_fonts(ctx);
        }
    }

    /// Applies the pending font set. New fonts apply from the next frame on: paint nothing until
    /// then. Startup always proceeds: without it a queued step would be re-offered forever.
    fn start_font_setup(&mut self, ctx: &egui::Context) {
        if self.fonts_set {
            self.fonts_ready = true;
            return;
        }
        if self.prepared_fonts {
            ctx.set_fonts(theme::pinned_font_definitions());
            theme::apply(ctx, self.ui.dark);
            self.fonts_set = true;
        } else {
            self.install_first_font_set(ctx);
        }
        self.fonts_set = true;
        // The rebuilt set has no workbook fonts yet: bind them again once it is active.
        self.fonts_bound.clear();
        self.fonts_scripts = crate::script::ScriptSet::EMPTY;
        self.fonts_seen = (0, 0);
        self.fonts_han = Some(theme::HanOrder::of(self.ui.language));
        ctx.request_repaint();
    }

    /// Builds the first font set from the open workbook, not only from the interface language.
    ///
    /// The background font scan is what the old path waited for: the first set knew the interface
    /// script but not the workbook's, so a Bengali or Ethiopic sheet opened on tofu glyphs and only
    /// recovered when the scan published the catalog a second or two later. Reading the scripts
    /// here costs one blocking `fc-list` (~50 ms on Linux) and draws the first frame correctly.
    fn install_first_font_set(&mut self, ctx: &egui::Context) {
        let used = self.used_fonts();
        let scripts = self.used_scripts();

        if scripts.is_empty() {
            // Nothing to draw yet (an empty workbook, or a fresh session): keep the non-blocking
            // startup path so opening the app does not wait for the font catalog.
            SheetApp::setup_context_for_language(ctx, self.ui.dark, self.ui.language);
            return;
        }
        let bold = self.used_bold_scripts();
        // Blocks until the catalog exists, so a script only the catalog can draw (Bengali, Ethiopic)
        // has its face in the first set instead of a frame of tofu.
        let _ = crate::fonts::catalog();
        let (mut defs, roles) = theme::font_definitions_roles(self.ui.language, &used, scripts, bold);
        if let Some(old) = &self.fonts_defs {
            theme::merge_fonts(&mut defs, old);
        }
        ctx.set_fonts(defs.clone());
        self.fonts_defs = Some(defs);
        self.pending_roles = roles;
        theme::apply(ctx, self.ui.dark);
        self.fonts_bound = used;
        self.fonts_scripts = scripts;
        self.fonts_seen = self.fonts_key();
    }

    /// Binds the workbook's installed fonts once the background scan has finished. Adding fonts
    /// re-applies the font set, which egui activates at the next pass, so skip this frame's paint
    /// (same one-frame cost as startup). Only when an open workbook changed.
    fn rebind_workbook_fonts(&mut self, ctx: &egui::Context) {
        let seen = self.fonts_key();
        if seen == self.fonts_seen {
            return;
        }
        let used = self.used_fonts();
        let scripts = self.used_scripts();
        self.fonts_seen = seen;
        // Text that gains a script (a Thai cell typed into a Latin sheet) needs its fallback face
        // even though every font name stayed the same.
        if used == self.fonts_bound && scripts == self.fonts_scripts {
            return;
        }
        let bold = self.used_bold_scripts();
        let (mut defs, roles) = theme::font_definitions_roles(self.ui.language, &used, scripts, bold);
        if let Some(old) = &self.fonts_defs {
            theme::merge_fonts(&mut defs, old);
        }
        ctx.set_fonts(defs.clone());
        self.fonts_defs = Some(defs);
        self.pending_roles = roles;
        self.fonts_bound = used;
        self.fonts_scripts = scripts;
        self.fonts_rebind = true;
        ctx.request_repaint();
    }
}
