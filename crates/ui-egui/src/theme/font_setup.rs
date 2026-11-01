#[cfg(not(target_arch = "wasm32"))]
use super::Blob;
use super::font_fallback;
use super::{Arc, Color32, FontData, FontDefinitions, FontFamily, FontId};

/// A font from the optional craft-fonts build input (`CRAFT_FONTS_DIR` at build time; see
/// <https://github.com/storytold/craft-fonts> and its `docs/integration.md`). The slice is empty
/// when the app is built without the checkout; nothing is downloaded and no font is in this repo.
#[derive(Debug)]
pub struct CraftFont {
    pub family: &'static str,
    pub style: &'static str,
    /// ISO 15924 scripts the family is intended for, from the craft-fonts manifest.
    pub scripts: &'static [&'static str],
    pub bytes: &'static [u8],
}

mod craft_fonts {
    use super::CraftFont;
    include!(concat!(env!("OUT_DIR"), "/craft_fonts.rs"));
}

/// The light or dark value of a colour, by mode.
pub(super) fn pick<T>(dark: bool, light: T, dark_value: T) -> T {
    if dark { dark_value } else { light }
}

pub const UI: &str = "ui";

pub const UI_BOLD: &str = "ui-bold";

pub const CELL: &str = "cell";

pub const CELL_BOLD: &str = "cell-bold";

pub const CELL_ITALIC: &str = "cell-italic";

pub const CELL_BOLD_ITALIC: &str = "cell-bold-italic";

pub const SERIF: &str = "serif";

pub const MONO: &str = "mono";

/// Equation faces: a maths font (upright letters, operators, Greek, double-struck and script
/// letters), its italic for variables, and a bold. System fonts only; nothing is bundled.
pub const MATH: &str = "math";

pub const MATH_ITALIC: &str = "math-italic";

pub const MATH_BOLD: &str = "math-bold";

pub fn ui_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(UI.into()))
}

pub fn ui_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(UI_BOLD.into()))
}

/// The font family to use for a cell font name and weight.
///
/// When the name is an installed font whose faces were bound by [`font_definitions`], that family
/// is used; otherwise the name is bucketed to one of the built-in roles (mono/serif/cell). egui
/// panics on a family that was never bound, so the installed branch is gated on
/// [`crate::fonts::is_bound`] and never names a family egui hasn't been given.
pub fn cell_family(name: &str, bold: bool, italic: bool) -> FontFamily {
    let role = crate::fonts::role_key(name, bold, italic);
    if crate::fonts::is_bound(&role) {
        return FontFamily::Name(role.into());
    }
    let lower = name.to_ascii_lowercase();
    if lower.contains("courier") || lower.contains("mono") || lower.contains("consolas") {
        return FontFamily::Name(MONO.into());
    }
    if lower.contains("times")
        || lower.contains("georgia")
        || lower.contains("cambria")
        || lower.contains("serif") && !lower.contains("sans")
        || lower.contains("garamond")
    {
        return FontFamily::Name(SERIF.into());
    }
    FontFamily::Name(
        match (bold, italic) {
            (true, true) => CELL_BOLD_ITALIC,
            (true, false) => CELL_BOLD,
            (false, true) => CELL_ITALIC,
            (false, false) => CELL,
        }
        .into(),
    )
}

/// Width in points, at 100% zoom, of one line of cell text drawn as the grid draws it: in the
/// face [`cell_family`] picks, which on a machine without the named font is a wider or narrower
/// substitute (#1316). A family the active font set doesn't have yet (bound fonts apply from the
/// next frame) falls back to the engine's estimate rather than letting egui panic on it.
///
/// `exact` lays the text out (a galley kept until the end of the frame); otherwise the glyph
/// advances are summed without kerning, which is cheap and never narrower. This takes the
/// Context lock (`fonts_mut`): never call it, or run a command, while an egui lock is held.
pub fn cell_text_width(ctx: &egui::Context, text: &str, font: &gridcraft_engine::model::Font, exact: bool) -> f32 {
    let family = cell_family(&font.name, font.bold, font.italic);
    let id = FontId::new(font.size * 96.0 / 72.0, family);
    ctx.fonts_mut(|f| {
        f.families().contains(&id.family).then(|| {
            if exact {
                f.layout_no_wrap(text.to_owned(), id, Color32::PLACEHOLDER).size().x
            } else {
                // Half a point covers the galley's rounding of its width up to the pixel grid.
                text.chars().map(|c| f.glyph_width(&id, c)).sum::<f32>() + 0.5
            }
        })
    })
    .unwrap_or_else(|| gridcraft_engine::cmd::format::approx_text_width(text, font.size, font.bold))
}

/// Whether epaint can open face `index` of this font file and it maps characters to glyphs.
/// epaint panics (in `Context::set_fonts`, i.e. on startup) on a face it can't open, so every font
/// file we did not ship is checked first with the same parser and the same call epaint makes
/// (`skrifa::FontRef::from_index`): a corrupt file, something that is not an sfnt at all, or a
/// collection index past the end is rejected instead of crashing the app (#226). A face whose
/// character map is unreadable (a truncated file) would open but draw nothing, so it is refused
/// too and the next candidate gets its turn.
pub(crate) fn font_parses(bytes: &[u8], index: u32) -> bool {
    use skrifa::MetadataProvider;
    skrifa::FontRef::from_index(bytes, index).is_ok_and(|font| font.charmap().has_map())
}

/// Loads the first candidate that exists and parses. A file that exists but can't be parsed is
/// skipped with a warning and the next candidate is tried.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn try_load(paths: &[(&str, u32)]) -> Option<FontData> {
    for (p, raw) in paths {
        // fontconfig packs a named instance into the high 16 bits; egui's face index is the low one.
        let (index, instance) = crate::fonts::fc_index_parts(*raw);
        if let Some(blob) = font_file(p, index) {
            let mut fd = FontData::from_blob(blob, index);
            if let Some(instance) = instance {
                crate::fonts::apply_named_instance(&mut fd, instance);
            }
            // A script face is a fallback in the workbook families that lack its script; without
            // this it is centred in their row instead of sitting on their baseline (#1231).
            crate::fonts::align_fallback(&mut fd, p);
            return Some(fd);
        }
    }
    None
}

/// The interface and script fallback font files, each read once per run and kept as a shared
/// `Blob` for the rest of it. egui shares the blob instead of copying the file into its text
/// engine, because every one of these files is loaded by every font rebuild for as long as the
/// app runs. Only a file that parses is kept: a missing or broken one is looked for again next
/// time.
#[cfg(not(target_arch = "wasm32"))]
fn font_file(path: &str, index: u32) -> Option<Blob> {
    static FILES: std::sync::Mutex<std::collections::BTreeMap<String, Blob>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
    let mut files = FILES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(blob) = files.get(path) {
        let bytes: &[u8] = (**blob).as_ref();
        return font_parses(bytes, index).then(|| std::sync::Arc::clone(blob));
    }
    let bytes = std::fs::read(path).ok()?;
    if !font_parses(&bytes, index) {
        log::warn!("skipping font {path} (face {index}): not a font file egui can read");
        return None;
    }
    let blob: Blob = std::sync::Arc::new(bytes);
    files.insert(path.to_string(), blob.clone());
    Some(blob)
}

/// Last line of defence before [`FontDefinitions`] reach egui: drops every face epaint could not
/// parse (and its key from every family), so a bad font costs a fallback, never a crash.
pub(crate) fn drop_unparseable_fonts(fonts: &mut FontDefinitions) {
    let bad: Vec<String> = fonts.font_data.iter().filter(|(_, fd)| !font_parses(fd.bytes(), fd.index)).map(|(k, _)| k.clone()).collect();
    for key in &bad {
        log::warn!("dropping font {key:?}: egui can't parse it");
        fonts.font_data.remove(key);
    }
    if !bad.is_empty() {
        for chain in fonts.families.values_mut() {
            chain.retain(|k| !bad.contains(k));
        }
    }
}

/// egui's bundled monospace face, used as the last-resort symbol fallback (see `Hack-Regular.ttf`
/// in `epaint_default_fonts`). It is present whenever egui is built with `default_fonts`.
const HACK: &str = "Hack";

/// The permitted craft-fonts faces with their `FontDefinitions` keys, in manifest order. Empty
/// without a `CRAFT_FONTS_DIR` checkout at build time. The filter is repeated here so a stale
/// generated table can never smuggle in a barred face or a web-build face outside the allowlist.
pub(super) fn craft_font_faces() -> Vec<(String, &'static CraftFont)> {
    let wasm = cfg!(target_arch = "wasm32");
    craft_fonts::CRAFT_FONTS
        .iter()
        .enumerate()
        .filter(|(_, font)| !crate::craft_fonts_policy::barred(font.family))
        .filter(|(_, font)| !wasm || crate::craft_fonts_policy::web_allowed(font.family, font.style))
        .map(|(i, font)| (format!("craft-{i}-{}-{}", font.family, font.style), font))
        .collect()
}

/// Adds the optional craft-fonts faces to `fonts` and returns their keys.
fn add_craft_fonts(fonts: &mut FontDefinitions) -> Vec<String> {
    let mut keys = Vec::new();
    for (key, font) in craft_font_faces() {
        let mut fd = FontData::from_static(font.bytes);
        crate::fonts::align_fallback(&mut fd, font.family);
        fonts.font_data.insert(key.clone(), Arc::new(fd));
        keys.push(key);
    }
    keys
}

/// Whether the optional craft-fonts input was embedded in this build.
pub fn craft_fonts_loaded() -> bool {
    !craft_fonts::CRAFT_FONTS.is_empty()
}

/// Whether the native-only Simplified Chinese craft face (LXGW WenKai Lite) was embedded. A web
/// build or an older craft-fonts checkout without `fonts/simplified-chinese-manifest.txt` has none.
pub fn chinese_font_loaded() -> bool {
    craft_font_faces().iter().any(|(_, f)| f.family == "LXGW WenKai Lite" && f.style == "Regular")
}

/// Whether this build was made with `CRAFT_FONTS_REQUIRED=1`. Acceptance tests must fail rather
/// than skip when this is true and the faces are absent.
pub fn craft_fonts_required() -> bool {
    craft_fonts::CRAFT_FONTS_REQUIRED
}

/// A bundled-face character with the same meaning as `c`, for interface chrome whose exact symbol
/// no bundled face has (`✕`, `✓`, `⌃`). Document text still needs a real glyph; this is only a
/// display fallback and never changes the characters a command inserts or stores.
pub fn symbol_fallback(c: char) -> Option<char> {
    match c {
        '✕' => Some('×'),
        '✓' => Some('✔'),
        '⌃' => Some('^'),
        _ => None,
    }
}

/// `text` with every character the current font set cannot draw replaced by its semantic fallback
/// (see [`symbol_fallback`]). Used by chrome tooltips so a host without symbol faces never paints
/// a tofu box; it is not applied to workbook text.
pub fn available_text(ui: &egui::Ui, text: &str) -> String {
    let font = ui_font(13.0);
    text.chars().map(|c| if ui.ctx().fonts_mut(|f| f.has_glyph(&font, c)) { c } else { symbol_fallback(c).unwrap_or(c) }).collect()
}

/// The family, style and scripts of every embedded craft-fonts face (empty without the input).
pub fn craft_font_scripts() -> impl Iterator<Item = (&'static str, &'static str, &'static [&'static str])> {
    craft_font_faces().into_iter().map(|(_, f)| (f.family, f.style, f.scripts))
}

/// Whether one embedded craft-fonts face maps every non-whitespace character in `text`. Tests use
/// this to tell "the pinned fixture covers this script" from "this host has no face for it",
/// without trusting a manifest script tag alone.
pub fn craft_fonts_cover(text: &str) -> bool {
    craft_font_faces().iter().any(|(_, f)| {
        use skrifa::MetadataProvider;
        let Ok(font) = skrifa::FontRef::from_index(f.bytes, 0) else {
            return false;
        };
        let map = font.charmap();
        text.chars().filter(|c| !c.is_whitespace()).all(|c| map.map(c).is_some())
    })
}

#[cfg(target_arch = "wasm32")]
pub(super) fn try_load(_paths: &[(&str, u32)]) -> Option<FontData> {
    None
}

fn home_fonts(file: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Library/Fonts/{file}")
}

/// Every file in `files` under every directory in `dirs`, directory first, in the same order a
/// nested loop over the directories would add them.
pub(super) fn in_dirs(dirs: &[&str], files: &[&str]) -> Vec<String> {
    dirs.iter().flat_map(|d| files.iter().map(move |f| format!("{d}/{f}"))).collect()
}

/// Candidate font files per role, in preference order.
/// The fontconfig pattern that names this role's design, on platforms that have fontconfig.
fn fontconfig_role(role: &str) -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    Some(match role {
        UI | CELL => "sans-serif",
        UI_BOLD | CELL_BOLD => "sans-serif:weight=bold",
        CELL_ITALIC => "sans-serif:slant=italic",
        CELL_BOLD_ITALIC => "sans-serif:weight=bold:slant=italic",
        SERIF => "serif",
        MONO => "monospace",
        _ => return None,
    })
}

pub(super) fn candidates(role: &str) -> Vec<(String, u32)> {
    let mac_sys = "/System/Library/Fonts";
    let mac_sup = "/System/Library/Fonts/Supplemental";
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    // ChromeOS's Linux container (Crostini) mounts the host's fonts under `chromeos/` (#1762).
    let lin = ["/usr/share/fonts/truetype", "/usr/share/fonts", "/usr/local/share/fonts", "/usr/share/fonts/TTF", "/usr/share/fonts/chromeos"];
    let mut v: Vec<(String, u32)> = Vec::new();
    let mut add = |s: String, i: u32| v.push((s, i));
    match role {
        "ui" => {
            add(format!("{mac_sys}/SFNS.ttf"), 0);
            add(format!("{mac_sys}/HelveticaNeue.ttc"), 0);
            add(format!("{win}\\segoeui.ttf"), 0);
            for p in in_dirs(
                &lin,
                &["cantarell/Cantarell-Regular.otf", "noto/NotoSans-Regular.ttf", "dejavu/DejaVuSans.ttf", "liberation/LiberationSans-Regular.ttf"],
            ) {
                add(p, 0);
            }
            // Fedora keeps each family in its own package folder (#54).
            add("/usr/share/fonts/abattis-cantarell-fonts/Cantarell-Regular.otf".into(), 0);
            add("/usr/share/fonts/google-noto/NotoSans-Regular.ttf".into(), 0);
            add("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf".into(), 0);
            add("/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf".into(), 0);
        }
        "ui-bold" => {
            add(format!("{mac_sys}/HelveticaNeue.ttc"), 1);
            add(format!("{win}\\segoeuib.ttf"), 0);
            for p in in_dirs(&lin, &["noto/NotoSans-Bold.ttf", "dejavu/DejaVuSans-Bold.ttf", "liberation/LiberationSans-Bold.ttf"]) {
                add(p, 0);
            }
            add("/usr/share/fonts/google-noto/NotoSans-Bold.ttf".into(), 0);
            add("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf".into(), 0);
            add("/usr/share/fonts/liberation-sans-fonts/LiberationSans-Bold.ttf".into(), 0);
        }
        "cell" | "cell-bold" | "cell-italic" | "cell-bold-italic" => {
            let (carlito, calibri_win, calibri_mac, arial_mac, arial_win, lib, dejavu) = match role {
                "cell" => {
                    ("Carlito-Regular.ttf", "calibri.ttf", "Calibri.ttf", "Arial.ttf", "arial.ttf", "LiberationSans-Regular.ttf", "DejaVuSans.ttf")
                }
                "cell-bold" => (
                    "Carlito-Bold.ttf",
                    "calibrib.ttf",
                    "Calibri Bold.ttf",
                    "Arial Bold.ttf",
                    "arialbd.ttf",
                    "LiberationSans-Bold.ttf",
                    "DejaVuSans-Bold.ttf",
                ),
                "cell-italic" => (
                    "Carlito-Italic.ttf",
                    "calibrii.ttf",
                    "Calibri Italic.ttf",
                    "Arial Italic.ttf",
                    "ariali.ttf",
                    "LiberationSans-Italic.ttf",
                    "DejaVuSans-Oblique.ttf",
                ),
                _ => (
                    "Carlito-BoldItalic.ttf",
                    "calibriz.ttf",
                    "Calibri Bold Italic.ttf",
                    "Arial Bold Italic.ttf",
                    "arialbi.ttf",
                    "LiberationSans-BoldItalic.ttf",
                    "DejaVuSans-BoldOblique.ttf",
                ),
            };
            // Calibri if the user has it installed system-wide (never from an Office bundle).
            add(format!("/Library/Fonts/{calibri_mac}"), 0);
            add(home_fonts(calibri_mac), 0);
            add(format!("{win}\\{calibri_win}"), 0);
            for p in in_dirs(&lin, &[&format!("crosextra/{carlito}"), &format!("carlito/{carlito}")]) {
                add(p, 0);
            }
            // Fedora keeps each family in its own package folder, as for the UI fonts (#54).
            add(format!("/usr/share/fonts/google-carlito-fonts/{carlito}"), 0);
            add(home_fonts(carlito), 0);
            add(format!("{mac_sup}/{arial_mac}"), 0);
            add(format!("{win}\\{arial_win}"), 0);
            for p in in_dirs(&lin, &[&format!("liberation/{lib}"), &format!("dejavu/{dejavu}")]) {
                add(p, 0);
            }
            add(format!("/usr/share/fonts/liberation-sans-fonts/{lib}"), 0);
            add(format!("/usr/share/fonts/dejavu-sans-fonts/{dejavu}"), 0);
        }
        "serif" => {
            add(format!("{mac_sup}/Times New Roman.ttf"), 0);
            add(format!("{win}\\times.ttf"), 0);
            for p in in_dirs(&lin, &["liberation/LiberationSerif-Regular.ttf", "dejavu/DejaVuSerif.ttf"]) {
                add(p, 0);
            }
            add("/usr/share/fonts/liberation-serif-fonts/LiberationSerif-Regular.ttf".into(), 0);
            add("/usr/share/fonts/dejavu-serif-fonts/DejaVuSerif.ttf".into(), 0);
        }
        "math" => {
            add(format!("{mac_sup}/STIXTwoMath.otf"), 0);
            add(format!("{win}\\cambria.ttc"), 1);
            for p in in_dirs(
                &lin,
                &[
                    "opentype/stix/STIXTwoMath-Regular.otf",
                    "stix/STIXTwoMath-Regular.otf",
                    "opentype/lmodern/latinmodern-math.otf",
                    "dejavu/DejaVuSerif.ttf",
                    "liberation/LiberationSerif-Regular.ttf",
                ],
            ) {
                add(p, 0);
            }
            add(format!("{mac_sup}/Times New Roman.ttf"), 0);
            add(format!("{win}\\times.ttf"), 0);
        }
        "math-italic" => {
            add(format!("{mac_sup}/STIXTwoText-Italic.ttf"), 0);
            add(format!("{mac_sup}/Times New Roman Italic.ttf"), 0);
            add(format!("{win}\\cambriai.ttf"), 0);
            add(format!("{win}\\timesi.ttf"), 0);
            for p in
                in_dirs(&lin, &["opentype/stix/STIXTwoText-Italic.otf", "liberation/LiberationSerif-Italic.ttf", "dejavu/DejaVuSerif-Italic.ttf"])
            {
                add(p, 0);
            }
        }
        "math-bold" => {
            add(format!("{mac_sup}/Times New Roman Bold.ttf"), 0);
            add(format!("{win}\\cambriab.ttf"), 0);
            add(format!("{win}\\timesbd.ttf"), 0);
            for p in in_dirs(&lin, &["opentype/stix/STIXTwoText-Bold.otf", "liberation/LiberationSerif-Bold.ttf", "dejavu/DejaVuSerif-Bold.ttf"]) {
                add(p, 0);
            }
        }
        "mono" => {
            add(format!("{mac_sys}/SFNSMono.ttf"), 0);
            add(format!("{mac_sys}/Menlo.ttc"), 0);
            add(format!("{win}\\consola.ttf"), 0);
            for p in in_dirs(&lin, &["dejavu/DejaVuSansMono.ttf"]) {
                add(p, 0);
            }
            add("/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf".into(), 0);
            // Fedora installs DejaVu Sans Mono only on request. Liberation Mono comes with LibreOffice
            // (in Workstation and KDE by default), Noto Sans Mono (variable, Regular by default) always.
            for p in in_dirs(&lin, &["liberation/LiberationMono-Regular.ttf"]) {
                add(p, 0);
            }
            add("/usr/share/fonts/liberation-mono-fonts/LiberationMono-Regular.ttf".into(), 0);
            add("/usr/share/fonts/google-noto-vf/NotoSansMono[wght].ttf".into(), 0);
        }
        _ => {}
    }
    v
}

/// Builds the font set from system fonts (with egui's defaults as fallback), with the default
/// (non-Japanese) Han order. Never reads the host locale, so tests and snapshots are stable.
pub fn font_definitions() -> FontDefinitions {
    font_definitions_for_language(crate::i18n::Language::En)
}

/// Which Han face leads the fallback chain. Both Han faces cover kana and most kanji, so this only
/// decides glyph shapes (and which face catches a glyph the other lacks); switching between two
/// languages with the same order needs no font rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HanOrder {
    /// Japanese glyph shapes first (the interface is Japanese).
    Japanese,
    /// Simplified Chinese first: the widest Han coverage, for every other interface language.
    Chinese,
}

impl HanOrder {
    pub fn of(language: crate::i18n::Language) -> Self {
        if language == crate::i18n::Language::Ja { Self::Japanese } else { Self::Chinese }
    }
}

/// Builds the font set with the Han fallback faces ordered for the interface language.
pub fn font_definitions_for_language(language: crate::i18n::Language) -> FontDefinitions {
    font_definitions_with(language, &[], crate::script::ScriptSet::EMPTY, crate::script::ScriptSet::EMPTY)
}

/// The bundled faces plus the pinned craft-fonts faces, with no system-font probing or installed
/// workbook-font binding.
///
/// Integration tests use this through their shared harness, so a minimal CI image and a developer
/// machine see the same font set. Craft-fonts remains a pinned build input rather than a repository
/// asset; without it only egui's bundled faces are available, which is the documented no-font mode.
pub fn pinned_font_definitions() -> FontDefinitions {
    font_definitions_impl(crate::i18n::Language::En, &[], crate::script::ScriptSet::EMPTY, crate::script::ScriptSet::EMPTY, false).0
}

/// [`font_definitions_for_language`] that also binds the workbook fonts in `used` that are
/// installed, so cells set in them render with them; the rest fall back to the roles. The bound
/// families are recorded in [`crate::fonts`] so [`cell_family`] only names a family egui has.
pub fn font_definitions_with(
    language: crate::i18n::Language,
    used: &[String],
    scripts: crate::script::ScriptSet,
    bold_scripts: crate::script::ScriptSet,
) -> FontDefinitions {
    let (fonts, roles) = font_definitions_roles(language, used, scripts, bold_scripts);
    crate::fonts::set_bound_for(&fonts, roles);
    fonts
}

/// [`font_definitions_with`] without publishing the role registry: the caller publishes it once
/// the set is active, so a frame painted with the previous set never names a family only the new
/// one carries (#1231).
pub fn font_definitions_roles(
    language: crate::i18n::Language,
    used: &[String],
    scripts: crate::script::ScriptSet,
    bold_scripts: crate::script::ScriptSet,
) -> (FontDefinitions, Vec<String>) {
    font_definitions_impl(language, used, scripts, bold_scripts, true)
}

/// Builds the role font chains, script fallbacks, installed workbook fonts and optional craft-font faces, drops unparseable data and records which families were bound.
fn font_definitions_impl(
    language: crate::i18n::Language,
    used: &[String],
    scripts: crate::script::ScriptSet,
    bold_scripts: crate::script::ScriptSet,
    system: bool,
) -> (FontDefinitions, Vec<String>) {
    let mut fonts = FontDefinitions::default();
    let base_prop: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let base_mono: Vec<String> = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mut bound: Vec<String> = Vec::new();
    // The non-Latin fallback faces have to sit at the end of each chain: a Latin font owns Latin,
    // and epaint picks the first face that has the glyph, so they only catch their own scripts.
    let han = HanOrder::of(language);
    let wanted = font_fallback::wanted_scripts(language, scripts);
    // A workbook that has not been scanned reports no scripts and is handed every face; there is
    // nothing to draw yet, so the catalog supplement waits for the first scan rather than loading a
    // face per installed script on startup.
    let known = !scripts.is_empty();
    let fallback = if system { font_fallback::load_fallback(&mut fonts, han, false, wanted, known) } else { Vec::new() };

    // Serif companions (Noto Serif CJK alone is ~55 MB) are loaded only when something asks for a
    // serif face: a workbook font whose name reads as serif, or the serif role. An installed
    // family that really carries a serif face asks for them too: its name need not say so.
    let installed = crate::fonts::installed_among(used);
    let want_serif = used.iter().any(|name| crate::fonts::family_name_is_serif(name))
        || installed.iter().any(|family| crate::fonts::faces_of(family).iter().any(|face| face.serif));
    let serif_fallback = if system && want_serif { font_fallback::load_fallback(&mut fonts, han, true, wanted, known) } else { Vec::new() };
    // Bold script faces, for the scripts the workbook actually sets in bold.
    let bold_sans = if system { font_fallback::load_bold_script_faces(&mut fonts, bold_scripts, false) } else { Vec::new() };
    let bold_serif = if system && want_serif { font_fallback::load_bold_script_faces(&mut fonts, bold_scripts, true) } else { Vec::new() };
    // A bold role wants the bold script faces, but only some scripts have one this host can read
    // (a variable collection's bold is a named instance `try_load` refuses). Keep the regular
    // faces behind them: a script without a bold face then draws at regular weight instead of
    // tofu (#1231).
    let bold_role_fallback = with_regular_faces(&bold_sans, &fallback);
    let bold_role_serif = with_regular_faces(&bold_serif, &serif_fallback);
    let hack = fonts.font_data.contains_key(HACK).then_some(HACK);
    for role in [UI, UI_BOLD, CELL, CELL_BOLD, CELL_ITALIC, CELL_BOLD_ITALIC, SERIF, MONO, MATH, MATH_ITALIC, MATH_BOLD] {
        // A bold role takes the bold script faces: a Calibri cell draws its Latin in Carlito Bold,
        // so its Han has to come from a bold CJK face or half the cell stays regular.
        let role_fallback = if role == SERIF && want_serif {
            &serif_fallback
        } else if matches!(role, UI_BOLD | CELL_BOLD | CELL_BOLD_ITALIC) && !bold_role_fallback.is_empty() {
            &bold_role_fallback
        } else if matches!(role, UI_BOLD | CELL_BOLD | CELL_BOLD_ITALIC) && !bold_role_serif.is_empty() {
            &bold_role_serif
        } else {
            &fallback
        };
        let chain = build_role_chain(&mut fonts, role, &base_prop, &base_mono, role_fallback, hack, system);
        fonts.families.insert(FontFamily::Name(role.into()), chain);
        bound.push(role.to_string());
    }
    // A serif family has to reach the serif script faces (Noto Serif CJK, Naskh, …): a Times New
    // Roman cell drawing its Latin in Liberation Serif and its Han in Noto Sans CJK looks like two
    // documents (#1231). The sans faces stay behind them, so a script this host ships only in sans
    // still draws instead of turning into tofu.
    let serif_then_sans = with_regular_faces(&serif_fallback, &fallback);
    let bold_serif_then_sans = with_regular_faces(&with_regular_faces(&bold_serif, &bold_sans), &fallback);
    let script_chains = |serif: bool| -> (&[String], &[String]) {
        if serif {
            (&serif_then_sans, &bold_serif_then_sans)
        } else {
            (&fallback, &bold_sans)
        }
    };
    // Installed workbook fonts, so a cell's chosen font actually renders in it.
    if system {
        for family in installed {
            let faces = crate::fonts::faces_of(&family);
            let (fallback, bold) = script_chains(faces.iter().any(|face| face.serif));
            bound.extend(crate::fonts::bind_faces(&mut fonts, &family, &faces, fallback, bold));
        }
        // A standard name this machine does not have is bound to the face the desktop substitutes
        // for it, under the name the workbook uses: Times New Roman cells keep bold and italic
        // even though only Liberation Serif is installed (#1231).
        let mut seen: Vec<String> = Vec::new();
        for name in used {
            let key = name.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let Some(resolved) = crate::fonts::substitute_family(name) else { continue };
            let faces = crate::fonts::faces_of(&resolved);
            if faces.is_empty() {
                continue;
            }
            // The substitute's own design decides: Times New Roman lands on Liberation Serif and
            // has to take the serif script faces with it (#1231).
            let (fallback, bold) = script_chains(faces.iter().any(|face| face.serif));
            bound.extend(crate::fonts::bind_faces(&mut fonts, name, &faces, fallback, bold));
        }
    }
    // Interface text (formula bar, name box, dialogs) falls back to the cell font before the
    // script faces: whatever a cell can draw, the formula bar can draw too, even where no script
    // fallback face was found (#349: macOS UI font without Hebrew, cells in Arial with it).
    bind_cell_fallback(&mut fonts, &fallback);
    // Default proportional text in widgets uses the UI font.
    if let Some(ui) = fonts.families.get(&FontFamily::Name(UI.into())).cloned() {
        fonts.families.insert(FontFamily::Proportional, ui);
    }
    // The optional craft-fonts faces go at the very end of every family: a system or bundled face
    // that has the script keeps its glyph shapes, while the permitted OFL faces cover scripts and
    // symbols the host image lacks (Japanese, Korean, Thai, Arabic, Georgian and the UI symbols).
    // Chinese comes from the native-only OFL LXGW face when embedded; a system Han face still
    // leads, and the barred Noto CJK collection never enters.
    append_craft_faces(&mut fonts);
    drop_unparseable_fonts(&mut fonts);
    // Only families egui still has data for count as bound (a dropped face can empty a chain).
    bound.retain(|f| fonts.families.get(&FontFamily::Name(f.as_str().into())).is_some_and(|c| !c.is_empty()));
    (fonts, bound)
}

/// Keep every family and face of `old` in `new`: a frame painted with the previous set stays valid
/// (no blank frame), and a family bound last frame can never go missing while the new set applies.
pub fn merge_fonts(new: &mut FontDefinitions, old: &FontDefinitions) {
    for (key, data) in &old.font_data {
        new.font_data.entry(key.clone()).or_insert_with(|| data.clone());
    }
    for (family, chain) in &old.families {
        let entry = new.families.entry(family.clone()).or_default();
        for key in chain {
            if !entry.contains(key) && new.font_data.contains_key(key) {
                entry.push(key.clone());
            }
        }
    }
}

/// Builds one role family's chain: the loaded system face (or its role fallback), egui's bundled
/// proportional/monospace base, Hack's symbols, then the script fallbacks. Keys without font data
/// are dropped, as before.
/// The bold script faces followed by the regular ones. A host often has a bold face for some
/// scripts only — a variable collection's bold is a named instance `try_load` refuses — and a
/// bold role that listed just the bold faces would draw those scripts as tofu (#1231).
fn with_regular_faces(bold: &[String], regular: &[String]) -> Vec<String> {
    let mut keys = bold.to_vec();
    for key in regular {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    keys
}

fn build_role_chain(
    fonts: &mut FontDefinitions,
    role: &str,
    base_prop: &[String],
    base_mono: &[String],
    fallback: &[String],
    hack: Option<&str>,
    system: bool,
) -> Vec<String> {
    // Linux asks fontconfig which file draws this role, instead of guessing from a path table:
    // the same answer the user's desktop gets, with bold and italic resolved by weight and slant.
    let paths = fontconfig_role(role).filter(|_| system).and_then(crate::fonts::fc_match).map(|p| vec![p]).unwrap_or_else(|| candidates(role));
    let refs: Vec<(&str, u32)> = paths.iter().map(|(p, i)| (p.as_str(), *i)).collect();
    let mut chain: Vec<String> = Vec::new();
    let loaded = if system { try_load(&refs) } else { None };
    if let Some(mut fd) = loaded {
        // Helvetica Neue Bold's own TrueType hints close Cyrillic е into ө at 13 pt on a Retina
        // screen (#1184). Core Text never runs them, so this draws it the way macOS does.
        if cfg!(target_os = "macos") && role == UI_BOLD {
            fd.tweak.hinting = Some(false);
        }
        let key = format!("sys-{role}");
        fonts.font_data.insert(key.clone(), Arc::new(fd));
        chain.push(key);
    } else if role == UI_BOLD {
        chain.push(format!("sys-{UI}"));
    } else if role.starts_with("cell-") {
        chain.push(format!("sys-{CELL}"));
    }
    // Equation faces fall back to the maths font (for the symbols a text face lacks), then to the
    // serif face.
    if role == MATH_ITALIC || role == MATH_BOLD {
        chain.push(format!("sys-{MATH}"));
    }
    if role.starts_with("math") {
        chain.push(format!("sys-{SERIF}"));
    }
    chain.extend(if role == MONO { base_mono.to_vec() } else { base_prop.to_vec() });
    // egui bundles Hack, a monospace face rich in arrows, triangles and keyboard symbols, but
    // 0.37 only puts it in the Monospace family. The interface draws ↑, ▲, ▼, ▸, ▾ and ↳ as
    // text (filter menus, ribbon, keytips), so add it behind the UI/cell and emoji faces; it
    // only catches characters those do not have.
    if let Some(hack) = hack
        && !chain.iter().any(|k| k.as_str() == hack)
    {
        chain.push(hack.to_string());
    }
    chain.extend(fallback.iter().cloned());
    chain.retain(|k| fonts.font_data.contains_key(k));
    chain
}

/// Inserts the loaded cell face before the script fallbacks of the interface families, so the
/// formula bar and dialogs can draw whatever a cell can (#349).
fn bind_cell_fallback(fonts: &mut FontDefinitions, fallback: &[String]) {
    let cell_key = format!("sys-{CELL}");
    if !fonts.font_data.contains_key(&cell_key) {
        return;
    }
    for role in [UI, UI_BOLD, SERIF, MONO] {
        if let Some(chain) = fonts.families.get_mut(&FontFamily::Name(role.into()))
            && !chain.contains(&cell_key)
        {
            let at = chain.iter().position(|k| fallback.contains(k)).unwrap_or(chain.len());
            chain.insert(at, cell_key.clone());
        }
    }
}

/// Appends the permitted craft-fonts faces to every family, preserving the leading system or
/// bundled face's glyph shapes.
fn append_craft_faces(fonts: &mut FontDefinitions) {
    let craft = add_craft_fonts(fonts);
    if craft.is_empty() {
        return;
    }
    for chain in fonts.families.values_mut() {
        for key in &craft {
            if !chain.contains(key) {
                chain.push(key.clone());
            }
        }
    }
}

#[cfg(test)]
mod font_binding_tests {
    use super::*;
    use crate::script::ScriptSet;

    /// The families [`font_definitions`] always binds, whatever is installed.
    fn always_bound() -> Vec<String> {
        [UI, UI_BOLD, CELL, CELL_BOLD, CELL_ITALIC, CELL_BOLD_ITALIC, SERIF, MONO].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_name_that_is_not_an_installed_font_gets_a_built_in_role() {
        let _g = crate::fonts::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Nothing installed: every name buckets to a role that font_definitions bound.
        crate::fonts::set_bound(always_bound());
        for name in ["", "Calibri", "Courier New", "Times New Roman", "Grossly Missing Font", "\u{1F600}"] {
            for (b, i) in [(false, false), (true, false), (false, true), (true, true)] {
                let FontFamily::Name(fam) = cell_family(name, b, i) else { panic!("{name:?} should name a family") };
                assert!(crate::fonts::is_bound(&fam), "{name:?} {b} {i} → unbound {fam:?}");
            }
        }
    }

    #[test]
    fn a_bound_installed_font_is_used_by_name() {
        let _g = crate::fonts::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let role = crate::fonts::role_key("Ubuntu", true, false);
        crate::fonts::set_bound(always_bound().into_iter().chain([role.clone()]));
        assert!(matches!(cell_family("Ubuntu", true, false), FontFamily::Name(f) if *f == *role));
        // The same family in a style that wasn't bound falls back to a built-in role that is.
        let FontFamily::Name(fallback) = cell_family("Ubuntu", false, true) else { panic!("should name a family") };
        assert!(crate::fonts::is_bound(&fallback), "the fallback is a bound role");
        crate::fonts::set_bound(always_bound());
    }

    #[test]
    fn a_serif_family_reaches_the_serif_script_faces() {
        // A Times New Roman cell draws its Latin in Liberation Serif; its Han, Arabic and Thai have
        // to come from serif faces too. The family's own design decides, not its name (#1231).
        let _g = crate::fonts::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let scripts = ScriptSet::EMPTY.with(crate::script::ScriptClass::Han);
        let (fonts, _) = font_definitions_impl(crate::i18n::Language::En, &["Times New Roman".to_string()], scripts, ScriptSet::EMPTY, true);
        let Some(chain) = fonts.families.get(&FontFamily::Name(crate::fonts::role_key("Times New Roman", false, false).into())).cloned() else {
            return; // no substitute for Times New Roman on this host
        };
        assert!(!chain.is_empty(), "the family was not bound: {chain:?}");
        let serif_han = ["sys-cjk-han-zh-serif", "sys-cjk-han-ja-serif"];
        let loaded: Vec<&str> = serif_han.iter().copied().filter(|key| fonts.font_data.contains_key(*key)).collect();
        if loaded.is_empty() {
            return; // this host has no serif CJK face to offer
        }
        let sans = chain.iter().position(|key| key == "sys-cjk-han-zh" || key == "sys-cjk-han-ja");
        for key in loaded {
            let at = chain.iter().position(|bound| bound == key).unwrap_or_else(|| panic!("{key} missing from {chain:?}"));
            if let Some(sans) = sans {
                // The sans face stays behind the serif one as a backstop for a script this host
                // ships in sans only.
                assert!(at < sans, "{key} is not ahead of the sans Han face in {chain:?}");
            }
        }
    }

    #[test]
    fn a_sans_family_keeps_the_sans_script_faces() {
        let _g = crate::fonts::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let scripts = ScriptSet::EMPTY.with(crate::script::ScriptClass::Han);
        let (fonts, _) = font_definitions_impl(crate::i18n::Language::En, &["Calibri".to_string()], scripts, ScriptSet::EMPTY, true);
        let Some(chain) = fonts.families.get(&FontFamily::Name(crate::fonts::role_key("Calibri", false, false).into())).cloned() else {
            return; // no substitute for Calibri on this host
        };
        assert!(!chain.iter().any(|key| key.starts_with("sys-") && key.ends_with("-serif")), "{chain:?}");
    }

    #[test]
    fn a_bold_role_keeps_the_regular_faces_behind_its_bold_ones() {
        // Bold script faces come first, but the regular ones stay in the chain: without them a
        // script whose bold face is unreadable would have no face at all and draw tofu (#1231).
        let bold = vec!["sys-script-han-bold".to_string(), "sys-script-arabic-bold".to_string()];
        let regular = vec!["sys-script-arabic-bold".to_string(), "sys-cjk-han-zh".to_string(), "sys-thai".to_string()];
        assert_eq!(with_regular_faces(&bold, &regular), ["sys-script-han-bold", "sys-script-arabic-bold", "sys-cjk-han-zh", "sys-thai"]);
        assert_eq!(with_regular_faces(&[], &regular), regular);
    }

    #[test]
    fn arbitrary_names_never_yield_an_unbound_family() {
        let _g = crate::fonts::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A workbook can hold any font name; none may name a family egui has no data for.
        crate::fonts::set_bound(always_bound());
        for seed in 0..500u32 {
            let name: String =
                (0..(seed % 40)).map(|k| char::from_u32(0x20 + (seed.wrapping_mul(31).wrapping_add(k) % 0x5F)).unwrap_or('x')).collect();
            let FontFamily::Name(fam) = cell_family(&name, seed % 2 == 0, seed % 3 == 0) else { continue };
            assert!(crate::fonts::is_bound(&fam), "{name:?} → unbound {fam:?}");
        }
    }
}
