//! The fonts installed on the machine, so the font list can offer them and cells can render with
//! them (#4).
//!
//! egui has no font discovery of its own and panics when asked for a family that was never bound
//! to font data. So we do both halves: scan the platform's font folders for family names (a read
//! of each font file's `name` table — damaged or unreadable files yield nothing, never a panic),
//! and bind the files a workbook actually uses into the families
//! [`theme::font_definitions`](crate::theme::font_definitions) assembles.
//!
//! Native only: on wasm there are no system fonts, every function here reports "nothing", and the
//! font list is the standard [`crate::ribbon::FONTS`] set.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::script::ScriptSet;

/// A font file is skipped by the scan past this size: a catalog only needs each face's name, and a
/// damaged or enormous file must not turn startup into a long read.
#[cfg(not(target_arch = "wasm32"))]
const MAX_FONT_FILE: u64 = 64 << 20;

/// Reads a font file of at most [`MAX_FONT_FILE`] bytes, even when it grows after it was found.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_font(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_FONT_FILE {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FONT_FILE + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FONT_FILE).then_some(bytes)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn read_font(_path: &std::path::Path) -> Option<Vec<u8>> {
    None
}

/// A face keeps at most this many family names, so a hostile `name` table can't grow the catalog.
#[cfg(not(target_arch = "wasm32"))]
const MAX_FAMILY_NAMES: usize = 16;

/// One face of one installed font file.
#[derive(Clone, Debug)]
pub struct Face {
    /// Typographic family name, e.g. `"Segoe UI"` (the English one when the font has it): what the
    /// font list shows.
    pub family: String,
    /// Every family name the face carries, `family` first, then each language's typographic and
    /// legacy family names (`"Malgun Gothic"`, `"맑은 고딕"`). A workbook may name the font by any of
    /// them: Korean Excel saves `맑은 고딕` (#1898).
    pub names: Vec<String>,
    /// The file the face lives in.
    pub path: PathBuf,
    /// Face index within the file (collections hold several, e.g. a family's Regular and Bold).
    pub index: u32,
    /// Weight implied by the face's style name (400 = regular).
    pub weight: f32,
    /// Whether the style name reads as italic or oblique.
    pub italic: bool,
    /// Whether the face's OS/2 panose declares a serif style.
    pub serif: bool,
    /// The scripts the face has glyphs for, as the font's charset declares them (a hit on any of
    /// [`crate::script::SAMPLES`]'s characters per class; see [`ScriptSet`]).
    pub scripts: ScriptSet,
}

/// The font folders of the platform (the system's and the user's). Empty on wasm.
pub fn system_font_dirs() -> Vec<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    {
        Vec::new()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
        if cfg!(target_os = "macos") {
            dirs.extend(["/System/Library/Fonts", "/System/Library/Fonts/Supplemental", "/Library/Fonts"].map(Into::into));
            if let Some(h) = &home {
                dirs.push(h.join("Library/Fonts"));
            }
        } else if cfg!(windows) {
            let root = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
            dirs.push(root.join("Fonts"));
            if let Some(l) = std::env::var_os("LOCALAPPDATA") {
                dirs.push(PathBuf::from(l).join("Microsoft\\Windows\\Fonts"));
            }
        } else {
            dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts", "/usr/share/fonts/TTF"].map(Into::into));
            if let Some(h) = &home {
                dirs.push(h.join(".fonts"));
                dirs.push(h.join(".local/share/fonts"));
            }
        }
        dirs
    }
}

/// Weight implied by a style name (`"Bold"` → 700, `"Light"` → 300).
#[cfg(not(target_arch = "wasm32"))]
fn style_weight(style: &str) -> f32 {
    let s: String = style.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    const TABLE: &[(&str, f32)] = &[
        ("hairline", 100.0),
        ("thin", 100.0),
        ("extralight", 200.0),
        ("ultralight", 200.0),
        ("light", 300.0),
        ("medium", 500.0),
        ("demibold", 600.0),
        ("semibold", 600.0),
        ("extrabold", 800.0),
        ("ultrabold", 800.0),
        ("bold", 700.0),
        ("black", 900.0),
        ("heavy", 900.0),
    ];
    TABLE.iter().find(|(k, _)| s.contains(k)).map(|(_, w)| *w).unwrap_or(400.0)
}

impl Face {
    /// Whether the face goes by `name` in any language.
    pub fn answers_to(&self, name: &str) -> bool {
        self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }
}

/// Whether a style name reads as italic or oblique.
#[cfg(not(target_arch = "wasm32"))]
fn style_italic(style: &str) -> bool {
    let s: String = style.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    s.contains("italic") || s.contains("oblique")
}

/// Every face in `data` as `(index, family, names, style)`, where `names` holds every family name
/// (see [`Face::names`]). Damaged data yields nothing, never a panic.
#[cfg(not(target_arch = "wasm32"))]
fn file_faces(data: &[u8]) -> Vec<(u32, String, Vec<String>, String, bool, ScriptSet)> {
    use skrifa::MetadataProvider as _;
    use skrifa::raw::{FileRef, TableProvider as _};
    use skrifa::string::StringId;

    let Ok(file) = FileRef::new(data) else { return Vec::new() };
    let family_ids = [StringId::TYPOGRAPHIC_FAMILY_NAME, StringId::FAMILY_NAME];
    let mut out = Vec::new();
    for (position, font) in file.fonts().enumerate() {
        let Ok(font) = font else { continue };
        let Ok(index) = u32::try_from(position) else { continue };
        let Some(family) = face_name(&font, &family_ids) else { continue };
        let mut names = vec![family.clone()];
        for name in family_ids.iter().flat_map(|id| font.localized_strings(*id)) {
            if names.len() >= MAX_FAMILY_NAMES {
                break;
            }
            let name = name.to_string().trim_matches('\0').trim().to_string();
            if !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                names.push(name);
            }
        }
        let style = face_name(&font, &[StringId::TYPOGRAPHIC_SUBFAMILY_NAME, StringId::SUBFAMILY_NAME]).unwrap_or_else(|| "Regular".into());
        let serif = font.os2().is_ok_and(|os2| is_serif_panose(os2.panose_10()));
        let scripts = ScriptSet::of_coverage(|c| font.charmap().map(c).is_some());
        out.push((index, family, names, style, serif, scripts));
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn face_name(font: &skrifa::FontRef<'_>, ids: &[skrifa::string::StringId]) -> Option<String> {
    use skrifa::MetadataProvider as _;
    ids.iter()
        .find_map(|id| font.localized_strings(*id).english_or_first())
        .map(|name| name.to_string().trim_matches('\0').trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Whether the OS/2 panose declaration says the face is serifed.
#[cfg(not(target_arch = "wasm32"))]
fn is_serif_panose(panose: &[u8]) -> bool {
    (panose.first() == Some(&2) && matches!(panose.get(1).copied(), Some(2..=10)))
        || (panose.first() == Some(&3) && matches!(panose.get(1).copied(), Some(3..=10)))
}

#[cfg(not(target_arch = "wasm32"))]
fn is_font_path(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc"))
}

/// Every face in `path` appended to `out`. Unsupported, unreadable or damaged files, and files
/// over [`MAX_FONT_FILE`], contribute nothing. The file is read once for its metadata; the bytes
/// are read again only when a face is bound.
#[cfg(not(target_arch = "wasm32"))]
fn scan_file(path: &std::path::Path, out: &mut Vec<Face>) {
    if !is_font_path(path) {
        return;
    }
    let Ok(metadata) = std::fs::metadata(path) else { return };
    if metadata.len() > MAX_FONT_FILE {
        log::warn!("skipping font {}: larger than {MAX_FONT_FILE} bytes", path.display());
        return;
    }
    let Some(data) = read_font(path) else { return };
    for (index, family, names, style, serif, scripts) in file_faces(&data) {
        out.push(Face { family, names, path: path.to_path_buf(), index, weight: style_weight(&style), italic: style_italic(&style), serif, scripts });
    }
}

/// Catalog every face under `dirs`, recursing into subfolders. A folder reached twice (through a
/// symlink) is read once, so a link back to a parent ends the walk.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan_dirs(dirs: &[PathBuf]) -> Vec<Face> {
    let mut out: Vec<Face> = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.to_vec();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    while let Some(dir) = stack.pop() {
        let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen_dirs.insert(key) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                scan_file(&path, &mut out);
            }
        }
    }
    out
}

/// Every face fontconfig knows, in the order it reports them. `fc-list` is the usual Linux font
/// source and covers distribution-specific directories that [`system_font_dirs`] cannot know. The
/// metadata avoids opening each file during startup; a file is read only when a family is bound.
#[cfg(target_os = "linux")]
fn fontconfig_faces() -> Vec<Face> {
    let Ok(output) = std::process::Command::new("fc-list")
        .env("LC_ALL", "C")
        .args(["-f", "%{file}\t%{index}\t%{family[0]}\t%{family}\t%{style[0]}\t%{weight}\t%{slant}\t%{charset}\n"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut seen: HashSet<(PathBuf, u32)> = HashSet::new();
    let mut faces: Vec<Face> = Vec::new();
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Some(face) = fontconfig_face(line) else { continue };
        if face.path.is_file() && seen.insert((face.path.clone(), face.index)) {
            faces.push(face);
        }
    }
    faces
}

/// One tab-separated `fc-list` record, or `None` for a blank or unusable line.
#[cfg(target_os = "linux")]
fn fontconfig_face(line: &[u8]) -> Option<Face> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut fields = line.split(|byte| *byte == b'\t');
    let file = fields.next()?;
    let index = fields.next()?;
    let family = fields.next()?;
    let names = fields.next()?;
    let style = fields.next()?;
    let weight = fields.next()?;
    let slant = fields.next()?;
    let charset = fields.next()?;
    if fields.next().is_some() || file.is_empty() || family.is_empty() {
        return None;
    }
    // A variable font also reports a range row for its default instance (`[80 200]`, empty
    // style). Only concrete named instances and static faces are usable as catalog entries.
    if style.is_empty() {
        return None;
    }
    let index: u32 = std::str::from_utf8(index).ok()?.trim().parse().ok()?;
    let weight = fc_weight(std::str::from_utf8(weight).ok()?.trim().parse().ok()?);
    let slant: u32 = std::str::from_utf8(slant).ok()?.trim().parse().unwrap_or(0);
    let family = std::str::from_utf8(family).ok()?.trim();
    let path = PathBuf::from(OsStr::from_bytes(file));
    if !is_font_path(&path) {
        return None;
    }
    // `%{family}` lists every name fontconfig knows for the face — localized names and aliases
    // included (`Malgun Gothic, 맑은 고딕`), which a workbook may use (#1898).
    let names = match std::str::from_utf8(names) {
        Ok(all) => family_names(family, all),
        Err(_) => vec![family.to_string()],
    };
    let scripts = ScriptSet::of_charset(&String::from_utf8_lossy(charset));
    Some(Face { family: family.to_string(), names, path, index, weight, italic: slant >= 100, serif: family_name_is_serif(family), scripts })
}

/// `family` first, then each comma-separated name in `all`, trimmed, deduplicated
/// case-insensitively and capped at [`MAX_FAMILY_NAMES`].
#[cfg(target_os = "linux")]
fn family_names(family: &str, all: &str) -> Vec<String> {
    let mut names = vec![family.to_string()];
    for name in all.split(',') {
        if names.len() >= MAX_FAMILY_NAMES {
            break;
        }
        let name = name.trim();
        if !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    }
    names
}

/// The egui weight for a fontconfig weight. fontconfig's constants follow the OpenType
/// convention (`FC_WEIGHT_REGULAR` 80, `FC_WEIGHT_BOLD` 200), not CSS numbers, and
/// `FC_WEIGHT_EXTRABOLD` (205) sits as close to Bold as to Black — without its own point it would
/// round down to 700 and tie with Bold.
#[cfg(any(target_os = "linux", test))]
fn fc_weight(value: u32) -> f32 {
    const POINTS: &[(u32, f32)] =
        &[(0, 100.0), (40, 200.0), (50, 300.0), (80, 400.0), (100, 500.0), (180, 600.0), (200, 700.0), (205, 800.0), (210, 900.0)];
    POINTS.iter().min_by_key(|(fc, _)| fc.abs_diff(value)).map(|(_, weight)| *weight).unwrap_or(400.0)
}

/// Whether a family draws emoji, symbols or icons rather than text.
pub fn is_symbol_family(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["emoji", "symbol", "dingbat", "icon", "webding", "wingding", "awesome"].iter().any(|word| lower.contains(word))
}

/// Where a face sits vertically when it draws for a family whose first font is another design.
///
/// Text faces go on the first font's baseline: epaint's default centres a fallback in the row, which
/// sinks CJK, Thai or Devanagari against the Latin beside it (#1231). Emoji and icon faces keep the
/// centred default, because their glyphs fill the whole row and would drop below the baseline.
pub fn fallback_align(family: &str) -> egui::epaint::text::FallbackAlign {
    if is_symbol_family(family) { egui::epaint::text::FallbackAlign::Center } else { egui::epaint::text::FallbackAlign::Baseline }
}

/// Sets [`fallback_align`] on a face built from `family`.
pub fn align_fallback(fd: &mut egui::FontData, family: &str) {
    fd.tweak.fallback_align = fallback_align(family);
}

/// Whether a family name reads as a serif family, for a face fontconfig describes without a panose.
pub(crate) fn family_name_is_serif(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("times")
        || lower.contains("georgia")
        || lower.contains("cambria")
        || lower.contains("serif") && !lower.contains("sans")
        || lower.contains("garamond")
}

#[cfg(target_os = "linux")]
thread_local! {
    static FC_MATCH_CACHE: std::cell::RefCell<std::collections::HashMap<String, Option<(String, u32)>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Asks fontconfig (`fc-match`, part of the standard Linux font stack) for the best face for
/// `pattern`. The answer is cached for the session: a match is a subprocess, and the same name is
/// asked for every frame a re-bind is considered.
#[cfg(target_os = "linux")]
/// Splits the index `fc-match`/`fc-list` report into the face index (low 16 bits) and the named
/// instance (high 16 bits, which fontconfig stores 1-based; `None` means the default instance).
pub(crate) fn fc_index_parts(raw: u32) -> (u32, Option<u32>) {
    (raw & 0xFFFF, (raw >> 16).checked_sub(1))
}

/// Applies a variable font's named instance through `FontTweak::coords`. `FontData::index` only
/// selects a face, so without this a variable font always renders its default instance: fontconfig
/// resolves `Noto Sans CJK SC:weight=bold` to a bold named instance, but egui would draw the
/// collection's default (Thin, 100) and refuse the face at the high index outright (#1231).
pub(crate) fn apply_named_instance(fd: &mut egui::FontData, instance: u32) {
    use skrifa::MetadataProvider as _;
    let bytes: &[u8] = fd.font.as_ref().as_ref();
    let Ok(font) = skrifa::FontRef::from_index(bytes, fd.index) else { return };
    let Some(named) = usize::try_from(instance).ok().and_then(|i| font.named_instances().get(i)) else {
        log::warn!("font face {:?} has no named instance {instance}", fd.index);
        return;
    };
    let mut coords = egui::epaint::text::VariationCoords::default();
    for (axis, value) in font.axes().iter().zip(named.user_coords()) {
        // `u32` is infallible for `IntoTag`; a malformed tag must not panic.
        coords.push(u32::from_be_bytes(axis.tag().into_bytes()), value);
    }
    fd.tweak.coords = coords;
}

pub(crate) fn fc_match(pattern: &str) -> Option<(String, u32)> {
    if let Some(answer) = FC_MATCH_CACHE.with(|cache| cache.borrow().get(pattern).cloned()) {
        return answer;
    }
    let answer = fc_match_uncounted(pattern);
    FC_MATCH_CACHE.with(|cache| {
        cache.borrow_mut().insert(pattern.to_owned(), answer.clone());
    });
    answer
}

#[cfg(target_os = "linux")]
fn fc_match_uncounted(pattern: &str) -> Option<(String, u32)> {
    let output = std::process::Command::new("fc-match").args(["-f", "%{file}\n%{index}\n", pattern]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut lines = stdout.lines();
    let file = lines.next()?.trim();
    if file.is_empty() || !std::path::Path::new(file).is_file() {
        return None;
    }
    // fontconfig stores a named instance in the high 16 bits (1-based, 0 = default) and the face
    // index in the low 16. Both are kept: `try_load` splits them, and the instance is applied
    // through `FontTweak::coords` so a variable font keeps the weight fontconfig matched.
    let index = lines.next().and_then(|line| line.trim().parse().ok()).unwrap_or(0);
    Some((file.to_owned(), index))
}

/// The installed faces, preferring fontconfig's list on Linux and falling back to the platform's
/// font folders when fontconfig is unavailable or empty.
fn scan_installed() -> Vec<Face> {
    #[cfg(target_os = "linux")]
    {
        let faces = fontconfig_faces();
        if !faces.is_empty() {
            return faces;
        }
    }
    scan_dirs(&system_font_dirs())
}

#[cfg(target_arch = "wasm32")]
pub fn scan_dirs(_dirs: &[PathBuf]) -> Vec<Face> {
    Vec::new()
}

static CATALOG: OnceLock<Vec<Face>> = OnceLock::new();

/// The catalog of installed faces, scanned on first use. Blocks until the scan finishes.
pub fn catalog() -> &'static [Face] {
    CATALOG.get_or_init(scan_installed)
}

/// The catalog if the scan has finished, else `None` — never blocks, for use while painting.
fn catalog_now() -> Option<&'static [Face]> {
    CATALOG.get().map(Vec::as_slice)
}

/// Start cataloging the installed fonts on a background thread, so the first frame doesn't wait for
/// it. A no-op on wasm and after the scan has started.
pub fn start_scan() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        if CATALOG.get().is_none() {
            // A failed spawn leaves the scan to the first blocking lookup.
            let _ = std::thread::Builder::new().name("font-scan".into()).spawn(|| {
                catalog();
            });
        }
    });
}

/// Whether the background scan has finished.
pub fn scan_done() -> bool {
    CATALOG.get().is_some()
}

/// The faces of one installed family, by any of its names, regular-weight first (the order roles
/// pick from).
pub fn faces_of(family: &str) -> Vec<Face> {
    catalog_now().map(|cat| faces_named(cat, family)).unwrap_or_default()
}

/// The faces in `cat` that go by `wanted`. When two families claim one name the winner is fixed,
/// not left to the scan's directory order: a family whose own (English) name it is wins, else the
/// family of the first claiming face by path and index. Only the faces that carry the name are
/// kept, so a legacy name (`"Arial Black"`, name ID 1) picks its own face out of its family.
fn faces_named(cat: &[Face], wanted: &str) -> Vec<Face> {
    let mut v: Vec<Face> = if cat.iter().any(|f| f.family.eq_ignore_ascii_case(wanted)) {
        cat.iter().filter(|f| f.family.eq_ignore_ascii_case(wanted)).cloned().collect()
    } else {
        let owner = cat.iter().filter(|f| f.answers_to(wanted)).min_by(|a, b| a.path.cmp(&b.path).then(a.index.cmp(&b.index)));
        match owner {
            Some(o) => cat.iter().filter(|f| f.family.eq_ignore_ascii_case(&o.family) && f.answers_to(wanted)).cloned().collect(),
            None => Vec::new(),
        }
    };
    v.sort_by(|a, b| a.weight.total_cmp(&b.weight).then(a.italic.cmp(&b.italic)).then(a.index.cmp(&b.index)));
    v
}

/// The subset of `names` that is installed, sorted and deduped. These are the families whose files
/// get bound for a workbook, under the name the workbook uses: that is the name
/// [`cell_family`](crate::theme::cell_family) looks up, so `맑은 고딕` binds as `맑은 고딕`, not as
/// the catalog's `Malgun Gothic`.
pub fn installed_among(names: &[String]) -> Vec<String> {
    catalog_now().map(|cat| installed_in(cat, names)).unwrap_or_default()
}

/// [`installed_among`] over a given catalog. Names dedupe exactly, not case-folded: a role key is
/// built from the cell's exact string, so `Calibri` and `calibri` each need their own binding (the
/// font data behind them is shared, see [`face_key`]).
fn installed_in(cat: &[Face], names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for wanted in names {
        if seen.insert(wanted.as_str()) && cat.iter().any(|f| f.answers_to(wanted)) {
            out.push(wanted.clone());
        }
    }
    out.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    out
}

/// The standard name `name` should be drawn with when it is not installed, resolved through
/// fontconfig. Only the names Excel's font list offers are substituted: fontconfig answers for any
/// string, so a typo would otherwise silently bind a random face.
#[cfg(target_os = "linux")]
pub fn substitute_family(name: &str) -> Option<String> {
    if !crate::ribbon::FONTS.iter().any(|f| f.eq_ignore_ascii_case(name)) {
        return None;
    }
    let cat = CATALOG.get()?;
    if cat.iter().any(|f| f.answers_to(name)) {
        return None; // the real font is installed: nothing to substitute
    }
    let (path, _) = fc_match(name)?;
    let path = PathBuf::from(path);
    let family = cat.iter().find(|f| f.path == path)?.family.clone();
    if family.eq_ignore_ascii_case(name) {
        return None;
    }
    Some(family)
}

/// macOS and Windows resolve the standard names themselves (the core fonts ship with them, and
/// Core Text and DirectWrite substitute where one is absent), so nothing extra is bound.
#[cfg(not(target_os = "linux"))]
pub fn substitute_family(_name: &str) -> Option<String> {
    None
}

/// The font list a combo box shows: the standard [`crate::ribbon::FONTS`] names first (what Excel
/// offers, and what we substitute), then every other installed family A–Z. Never blocks: until the
/// background scan finishes it returns just the standard names.
pub fn families() -> Vec<String> {
    let mut out: Vec<String> = crate::ribbon::FONTS.iter().map(|s| s.to_string()).collect();
    let Some(cat) = catalog_now() else { return out };
    let mut seen: HashSet<String> = out.iter().map(|s| s.to_lowercase()).collect();
    let mut installed: Vec<String> = cat.iter().map(|f| f.family.clone()).collect();
    installed.sort_by_key(|a| a.to_lowercase());
    installed.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    for f in installed {
        if seen.insert(f.to_lowercase()) {
            out.push(f);
        }
    }
    out
}

/// The egui family name for one role of one family: `fnt:{family}:{b|n}{i|n}`.
pub fn role_key(family: &str, bold: bool, italic: bool) -> String {
    format!("fnt:{family}:{}{}", if bold { 'b' } else { 'n' }, if italic { 'i' } else { 'n' })
}

/// The face to use for a role, or `None` when the family has no faces. Missing bold/italic faces
/// fall back to the family's regular face, so every bound role has data behind it.
///
/// Within the faces that match a role, the one nearest the role's weight wins rather than the first
/// one the host lists: most families carry a Semibold between Regular and Bold, and for italic a
/// thin or light cut is a different design, not a slanted regular.
fn pick_face(faces: &[Face], bold: bool, italic: bool) -> Option<&Face> {
    let regular = || faces.iter().find(|f| !f.italic && (f.weight - 400.0).abs() < 1.0);
    let exact = |wanted_bold: bool, wanted_italic: bool| {
        let target = if wanted_bold { 700.0 } else { 400.0 };
        faces
            .iter()
            .filter(|f| f.italic == wanted_italic && (f.weight >= 600.0) == wanted_bold)
            .min_by(|a, b| (a.weight - target).abs().total_cmp(&(b.weight - target).abs()))
    };
    (match (bold, italic) {
        (true, true) => exact(true, true).or_else(|| exact(true, false)).or_else(|| exact(false, true)),
        (true, false) => exact(true, false),
        (false, true) => exact(false, true),
        (false, false) => None,
    })
    .or_else(regular)
    .or_else(|| faces.first())
}

/// A stable key for a face's font data, one per file and index: every name a family is bound under
/// (`Malgun Gothic`, `맑은 고딕`) shares the data instead of loading the file again.
fn face_key(face: &Face) -> String {
    format!("fnt-src:{}:{}", face.path.display(), face.index)
}

/// Bind one installed family's faces into `fonts` under `family` (the name the workbook uses), one
/// egui family per cell role, and return the
/// role keys that are now bound. A role with no exact face chains to the family's regular face, so
/// no role is ever left pointing at missing data (egui panics on that). A family with no faces
/// binds nothing.
///
/// Each role's chain ends with egui's default proportional fonts, so glyphs the installed font
/// lacks (emoji, a stray script) still render.
pub fn bind_faces(fonts: &mut egui::FontDefinitions, family: &str, faces: &[Face], fallback: &[String], bold_fallback: &[String]) -> Vec<String> {
    if faces.is_empty() {
        return Vec::new();
    }
    let base: Vec<String> = fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
    let mut bound: Vec<String> = Vec::new();
    for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
        let role = role_key(family, bold, italic);
        // What the family's real italic faces can draw: no italic file at all, or one whose italic
        // covers only Latin, leaves the rest of the text to the renderer's slant (#1231).
        if italic {
            set_italic_coverage(&role, Some(faces.iter().filter(|face| face.italic).fold(ScriptSet::EMPTY, |set, face| set.union(face.scripts))));
        }
        let Some(primary) = pick_face(faces, bold, italic) else { continue };
        let mut chain: Vec<String> = Vec::new();
        for face in [Some(primary), pick_face(faces, false, false)] {
            let Some(face) = face else { continue };
            let key = face_key(face);
            if chain.contains(&key) {
                continue;
            }
            if !fonts.font_data.contains_key(&key) {
                let Some(bytes) = read_font(&face.path) else { continue };
                let (index, instance) = fc_index_parts(face.index);
                let mut fd = egui::FontData::from_owned(bytes);
                fd.index = index;
                if let Some(instance) = instance {
                    apply_named_instance(&mut fd, instance);
                }
                align_fallback(&mut fd, &face.family);
                fonts.font_data.insert(key.clone(), std::sync::Arc::new(fd));
            }
            chain.push(key);
        }
        if chain.is_empty() {
            continue;
        }
        // A workbook font's role chain needs the script faces too: Carlito has no Han, so a Calibri
        // cell would draw tofu without them (#1231).
        // With no bold script face at all, the regular ones are the whole fallback: a bold cell
        // would otherwise draw every script beyond the family's own Latin as tofu (#1231).
        chain.extend(if bold && !bold_fallback.is_empty() { bold_fallback } else { fallback }.iter().cloned());
        // A script with no bold face still draws with its regular one rather than tofu.
        if bold && !bold_fallback.is_empty() {
            for key in fallback {
                if !chain.contains(key) {
                    chain.push(key.clone());
                }
            }
        }
        chain.extend(base.iter().cloned());
        fonts.families.insert(egui::FontFamily::Name(role.clone().into()), chain);
        bound.push(role);
    }
    bound
}

/// Bind every installed face of `family`, found by any of its names (see [`bind_faces`]).
pub fn bind_family(fonts: &mut egui::FontDefinitions, family: &str, fallback: &[String], bold_fallback: &[String]) -> Vec<String> {
    bind_faces(fonts, family, &faces_of(family), fallback, bold_fallback)
}

/// The role families currently bound to font data. `cell_family` consults this, so it never names a
/// family egui hasn't been given.
static BOUND: OnceLock<std::sync::RwLock<std::collections::HashMap<String, Option<ScriptSet>>>> = OnceLock::new();

fn bound() -> &'static std::sync::RwLock<std::collections::HashMap<String, Option<ScriptSet>>> {
    BOUND.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()))
}

/// Record the role families a freshly built [`egui::FontDefinitions`] bound, with the scripts their
/// italic face covers.
pub fn set_bound(keys: impl IntoIterator<Item = String>) {
    set_bound_for_roles(keys.into_iter().map(|key| (key, None)));
}

/// [`set_bound`] from a finished font set: only the roles the definitions actually carry are
/// recorded, so a role the old set advertised and the new one dropped cannot be named.
pub fn set_bound_for(defs: &egui::FontDefinitions, roles: Vec<String>) {
    set_bound_for_roles(roles.into_iter().filter_map(|role| {
        let present = defs.families.get(&egui::FontFamily::Name(role.as_str().into())).is_some_and(|c| !c.is_empty());
        present.then_some((role, None))
    }));
}

fn set_bound_for_roles(entries: impl Iterator<Item = (String, Option<ScriptSet>)>) {
    let mut next = std::collections::HashMap::new();
    let previous = bound().read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    for (role, coverage) in entries {
        // `bind_faces` records the italic coverage as it binds, just before this runs: keep it.
        next.insert(role.clone(), coverage.or_else(|| previous.get(&role).copied().flatten()));
    }
    *bound().write().unwrap_or_else(std::sync::PoisonError::into_inner) = next;
}

/// The scripts a role's real italic face covers. `None` means the family has no italic face at all,
/// so every script has to be slanted; `Some(set)` means only the scripts outside `set` are.
pub fn italic_coverage(family: &egui::FontFamily) -> Option<Option<ScriptSet>> {
    let egui::FontFamily::Name(name) = family else { return None };
    bound().read().unwrap_or_else(std::sync::PoisonError::into_inner).get(name.as_ref()).copied()
}

pub fn set_italic_coverage(role: &str, coverage: Option<ScriptSet>) {
    bound().write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(role.to_string(), coverage);
}

/// Is this exact role family bound to font data?
pub fn is_bound(role: &str) -> bool {
    bound().read().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(role)
}

/// Serializes tests that mutate the process-wide bound-family registry.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fontconfig_index_splits_into_face_and_instance() {
        assert_eq!(fc_index_parts(0), (0, None));
        assert_eq!(fc_index_parts(3), (3, None));
        // `fc-list` reports Noto Sans CJK's bold as face 1, named instance 6 stored 1-based.
        assert_eq!(fc_index_parts(0x6_0001), (1, Some(5)));
    }

    #[test]
    fn style_names_map_to_weight_and_slant() {
        assert_eq!(style_weight("Bold"), 700.0);
        assert_eq!(style_weight("SemiBold Italic"), 600.0);
        assert_eq!(style_weight("Regular"), 400.0);
        assert_eq!(style_weight("Light Oblique"), 300.0);
        assert!(style_italic("Bold Italic") && style_italic("Oblique"));
        assert!(!style_italic("Bold"));
        // The weight table is checked in order, so "extralight" wins over "light".
        assert_eq!(style_weight("ExtraLight"), 200.0);
    }

    #[test]
    fn text_faces_sit_on_the_baseline_and_symbol_faces_stay_centred() {
        use egui::epaint::text::FallbackAlign;
        // A fallback face's default is `Center`, which sinks CJK against the Latin beside it (#1231).
        assert_eq!(fallback_align("Noto Sans CJK SC"), FallbackAlign::Baseline);
        assert_eq!(fallback_align("Carlito"), FallbackAlign::Baseline);
        assert_eq!(fallback_align("Noto Sans Thai"), FallbackAlign::Baseline);
        assert_eq!(fallback_align("Noto Sans Symbols 2"), FallbackAlign::Center);
        assert_eq!(fallback_align("Noto Color Emoji"), FallbackAlign::Center);
        assert_eq!(fallback_align("Font Awesome 6 Free"), FallbackAlign::Center);
        // A face keeps its own family's rule wherever it is used.
        let mut fd = egui::FontData::from_owned(vec![0, 1, 0, 0, 0, 0xFF, 0, 0]);
        align_fallback(&mut fd, "Noto Color Emoji");
        assert_eq!(fd.tweak.fallback_align, FallbackAlign::Center);
        align_fallback(&mut fd, "Noto Sans Devanagari");
        assert_eq!(fd.tweak.fallback_align, FallbackAlign::Baseline);
    }

    #[test]
    fn role_keys_are_stable_and_distinct() {
        assert_eq!(role_key("Segoe UI", false, false), "fnt:Segoe UI:nn");
        assert_eq!(role_key("Segoe UI", true, false), "fnt:Segoe UI:bn");
        assert_eq!(role_key("Segoe UI", false, true), "fnt:Segoe UI:ni");
        assert_eq!(role_key("Segoe UI", true, true), "fnt:Segoe UI:bi");
    }

    #[test]
    fn nothing_is_bound_until_font_definitions_says_so() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A role key is only safe to name after font_definitions bound it.
        let k = role_key("Some Installed Font", true, false);
        assert!(!is_bound(&k));
        set_bound([k.clone()]);
        assert!(is_bound(&k));
        set_bound([]);
        assert!(!is_bound(&k));
    }

    #[test]
    fn damaged_or_non_font_files_contribute_nothing() {
        assert!(file_faces(b"ttcf\x00\x01\x00\x00\xff\xff\xff\xff").is_empty());
        assert!(file_faces(b"not a font at all").is_empty());
        assert!(file_faces(&[]).is_empty());
    }

    /// fontconfig weights follow the OpenType convention, not CSS numbers; `FC_WEIGHT_EXTRABOLD`
    /// (205) needs its own point or it ties with Bold (#1231).
    #[test]
    fn fc_weight_maps_the_fontconfig_scale() {
        assert_eq!(fc_weight(80), 400.0);
        assert_eq!(fc_weight(200), 700.0);
        assert_eq!(fc_weight(205), 800.0);
        assert_eq!(fc_weight(180), 600.0);
        assert_eq!(fc_weight(50), 300.0);
        assert_eq!(fc_weight(9999), 900.0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fontconfig_records_carry_family_style_and_slant() {
        use crate::script::ScriptClass;
        let bold = fontconfig_face(
            b"/usr/share/fonts/x/NotoSans-Bold.ttf\t2\tNoto Sans\tNoto Sans,Noto Sans Bold\tBold\t200\t0\t20-7e 4e00-9fff 1f600-1f64f",
        )
        .expect("a concrete face");
        assert_eq!(bold.family, "Noto Sans");
        assert_eq!(bold.index, 2);
        assert_eq!(bold.weight, 700.0);
        assert!(!bold.italic);
        assert!(bold.scripts.has(ScriptClass::Latin) && bold.scripts.has(ScriptClass::Han), "the charset names the scripts");
        assert!(bold.scripts.has(ScriptClass::Symbols), "an emoji block covers the symbol samples");
        assert!(!bold.scripts.has(ScriptClass::Thai), "a script absent from the charset stays out");
        assert_eq!(bold.names, vec!["Noto Sans".to_string(), "Noto Sans Bold".to_string()]);

        let italic =
            fontconfig_face(b"/usr/share/fonts/x/NotoSans-Italic.ttf\t0\tNoto Sans\tNoto Sans\tItalic\t80\t100\t20-7e").expect("an italic face");
        assert!(italic.italic);

        // The variable font's default-instance range row has no style and is not a usable face.
        assert!(fontconfig_face(b"/usr/share/fonts/x/NotoSans[wdth,wght].ttf\t0\tNoto Sans\tNoto Sans\t\t[80 200]\t0\t20-7e").is_none());
        // Truncated records and non-font files are ignored.
        assert!(fontconfig_face(b"/usr/share/fonts/x/NotoSans.ttf\t0\tNoto Sans").is_none());
        assert!(fontconfig_face(b"/usr/share/fonts/x/README.txt\t0\tNoto Sans\tNoto Sans\tRegular\t80\t0\t20-7e").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fontconfig_faces_are_existing_supported_fonts() {
        let faces = fontconfig_faces();
        // A minimal container may not have fontconfig or any fonts; the folder scan is the
        // fallback in that case. When fc-list does answer, every face must be usable.
        if faces.is_empty() {
            return;
        }
        assert!(faces.iter().all(|face| face.path.is_file() && is_font_path(&face.path)));
        assert!(faces.iter().all(|face| (100.0..=900.0).contains(&face.weight)));
        // `%{charset}` has to arrive for every record, so a Latin face says so.
        assert!(faces.iter().any(|face| face.scripts.has(crate::script::ScriptClass::Latin)), "charset parsed");
        let unique: HashSet<(&PathBuf, u32)> = faces.iter().map(|face| (&face.path, face.index)).collect();
        assert_eq!(unique.len(), faces.len(), "fc-list duplicates are removed");
    }

    /// A standard Excel name that is not installed resolves through fontconfig to the family that
    /// stands in for it; names outside the standard list never do, because fontconfig answers any
    /// pattern and binding a misspelt name would hide the miss (#1231).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_standard_name_resolves_to_its_substitute_family() {
        let cat = catalog();
        assert_eq!(substitute_family("DejaVu Sans"), None, "only standard names are substituted");
        assert_eq!(substitute_family("Grossly Missing Font"), None);
        if cat.iter().any(|f| f.answers_to("Times New Roman")) {
            return; // the real font is installed: nothing to substitute
        }
        // A container without fontconfig answers nothing, and the folder scan may find no
        // substitute either; when fontconfig does answer, the substitute must be usable.
        let Some(resolved) = substitute_family("Times New Roman") else { return };
        assert!(!resolved.eq_ignore_ascii_case("Times New Roman"));
        let faces = faces_of(&resolved);
        assert!(!faces.is_empty(), "{resolved} is in the catalog");
        assert!(faces.iter().any(|f| f.weight >= 600.0 && !f.italic), "the substitute has a bold face");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_scan_catalogs_fonts_and_reads_each_folder_once() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Sub")).expect("temp dir");
        std::fs::write(dir.join("Ubuntu-Light.ttf"), epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        std::fs::write(dir.join("damaged.ttf"), b"ttcf\x00\x01\x00\x00\xff\xff\xff\xff").expect("write");
        std::fs::write(dir.join("readme.txt"), b"not a font").expect("write");
        // The same folder listed twice (once through `Sub/..`) is read once.
        let faces = scan_dirs(&[dir.clone(), dir.join("Sub/..")]);
        let families: Vec<&str> = faces.iter().map(|f| f.family.as_str()).collect();
        assert_eq!(families, ["Ubuntu"], "one face, family read from the name table");
        assert_eq!(faces[0].weight, 300.0, "“Light” reads as weight 300");
        assert!(!faces[0].italic);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_back_to_a_parent_folder_ends_the_scan() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Sub")).expect("temp dir");
        std::fs::write(dir.join("Sub/Ubuntu-Light.ttf"), epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        std::os::unix::fs::symlink(&dir, dir.join("Sub/loop")).expect("symlink");
        assert_eq!(scan_dirs(std::slice::from_ref(&dir)).len(), 1, "the loop doesn't multiply faces");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn binding_a_family_binds_all_four_roles_and_never_stores_missing_data() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-bind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Ubuntu-Light.ttf");
        std::fs::write(&path, epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        let face = Face {
            family: "Ubuntu".into(),
            names: vec!["Ubuntu".into()],
            path,
            index: 0,
            weight: 300.0,
            italic: false,
            serif: false,
            scripts: ScriptSet::of_text("A"),
        };

        let mut fonts = egui::FontDefinitions::default();
        let bound = bind_faces(&mut fonts, "Ubuntu", std::slice::from_ref(&face), &[], &[]);
        assert_eq!(bound.len(), 4, "regular, bold, italic and bold-italic roles");
        for key in &bound {
            let chain = fonts.families.get(&egui::FontFamily::Name(key.as_str().into())).expect("role family");
            assert!(chain.iter().all(|k| fonts.font_data.contains_key(k)), "every chained key has data");
        }
        // A family with no faces binds nothing and never touches the map.
        assert!(bind_faces(&mut fonts, "No Such Font", &[], &[], &[]).is_empty());
        std::fs::File::create(&face.path).expect("sparse file").set_len(MAX_FONT_FILE + 1).expect("oversized file");
        assert!(read_font(&face.path).is_none(), "an oversized font is not read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A font that is just a table directory and a `name` table holding `records` as
    /// `(Windows language id, name id, text)`: all [`file_faces`] reads.
    #[cfg(not(target_arch = "wasm32"))]
    fn font_with_names(records: &[(u16, u16, &str)]) -> Vec<u8> {
        fn be(v: &mut Vec<u8>, x: u16) {
            v.extend_from_slice(&x.to_be_bytes());
        }
        let mut name: Vec<u8> = Vec::new();
        let mut strings: Vec<u8> = Vec::new();
        for x in [0, records.len() as u16, (6 + 12 * records.len()) as u16] {
            be(&mut name, x);
        }
        for (language, id, text) in records {
            let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
            // Platform 3 (Windows), encoding 1 (Unicode BMP).
            for x in [3, 1, *language, *id, utf16.len() as u16, strings.len() as u16] {
                be(&mut name, x);
            }
            strings.extend(utf16);
        }
        name.extend(strings);
        let mut font: Vec<u8> = 0x0001_0000u32.to_be_bytes().to_vec();
        for x in [1, 16, 0, 0] {
            be(&mut font, x);
        }
        font.extend(b"name");
        font.extend(0u32.to_be_bytes());
        font.extend(28u32.to_be_bytes());
        font.extend((name.len() as u32).to_be_bytes());
        font.extend(name);
        font
    }

    const EN: u16 = 0x0409;
    const KO: u16 = 0x0412;

    #[test]
    fn a_family_records_which_scripts_its_italic_covers() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let regular = face("Upright Only", &["Upright Only"], "/nonexistent/Upright.ttf", 400.0);
        let mut fonts = egui::FontDefinitions::default();
        bind_faces(&mut fonts, "Upright Only", std::slice::from_ref(&regular), &[], &[]);
        let italic = egui::FontFamily::Name(role_key("Upright Only", false, true).into());
        assert_eq!(italic_coverage(&italic), Some(Some(ScriptSet::EMPTY)), "no italic file, so every script is slanted");
        let roman = egui::FontFamily::Name(role_key("Upright Only", false, false).into());
        assert_eq!(italic_coverage(&roman), None, "upright text is never slanted");
        assert_eq!(italic_coverage(&egui::FontFamily::Proportional), None, "built-in families are not slanted");
        // A Latin-only italic (Carlito Italic behind Calibri) leaves Han to the renderer's slant.
        let mut slanted = regular.clone();
        slanted.italic = true;
        slanted.scripts = ScriptSet::of_text("A");
        let mut fonts = egui::FontDefinitions::default();
        bind_faces(&mut fonts, "Latin Italic", std::slice::from_ref(&slanted), &[], &[]);
        let real = egui::FontFamily::Name(role_key("Latin Italic", false, true).into());
        let coverage = italic_coverage(&real).flatten().expect("italic role");
        assert!(coverage.has(crate::script::ScriptClass::Latin), "the real italic face draws Latin");
        assert!(!coverage.has(crate::script::ScriptClass::Han), "Han has no italic face, so it is slanted");
    }

    #[test]
    fn a_role_takes_the_face_nearest_its_weight() {
        // Many families ship a Semibold between Regular and Bold, and a Light Italic next to an
        // Italic; picking the first match drew bold text in Semibold (#1231).
        let mut faces = vec![
            face("Noto Sans", &["Noto Sans"], "/f/regular.ttf", 400.0),
            face("Noto Sans", &["Noto Sans"], "/f/semibold.ttf", 600.0),
            face("Noto Sans", &["Noto Sans"], "/f/bold.ttf", 700.0),
        ];
        let bold = pick_face(&faces, true, false).map(|f| f.path.to_string_lossy().to_string());
        assert_eq!(bold.as_deref(), Some("/f/bold.ttf"));

        let mut italic =
            vec![face("Noto Sans", &["Noto Sans"], "/f/italic.ttf", 400.0), face("Noto Sans", &["Noto Sans"], "/f/light-italic.ttf", 300.0)];
        italic[0].italic = true;
        italic[1].italic = true;
        let slanted = pick_face(&italic, false, true).map(|f| f.path.to_string_lossy().to_string());
        assert_eq!(slanted.as_deref(), Some("/f/italic.ttf"));

        faces.clear();
        assert!(pick_face(&faces, true, false).is_none(), "a family with no faces picks nothing");
    }

    fn face(family: &str, names: &[&str], path: &str, weight: f32) -> Face {
        Face {
            family: family.into(),
            names: names.iter().map(|s| s.to_string()).collect(),
            path: path.into(),
            index: 0,
            weight,
            italic: false,
            serif: false,
            scripts: ScriptSet::of_text("A"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_face_keeps_its_family_names_in_every_language() {
        // Korean Excel saves `맑은 고딕`, so the Korean name has to be kept next to the English one (#1898).
        let font = font_with_names(&[(EN, 1, "Malgun Gothic"), (EN, 2, "Regular"), (KO, 1, "맑은 고딕"), (KO, 2, "보통")]);
        let faces = file_faces(&font);
        assert_eq!(faces.len(), 1);
        let (_, family, names, style, _, _) = &faces[0];
        assert_eq!(family, "Malgun Gothic", "the font list keeps the English name");
        assert_eq!(names, &["Malgun Gothic", "맑은 고딕"]);
        assert_eq!(style, "Regular");

        // Both name IDs are kept: a legacy family name (ID 1) names a single face (#1606).
        let font = font_with_names(&[(EN, 1, "Noto Sans KR Medium"), (EN, 2, "Regular"), (EN, 16, "Noto Sans KR"), (EN, 17, "Medium")]);
        let (_, family, names, _, _, _) = &file_faces(&font)[0];
        assert_eq!(family, "Noto Sans KR");
        assert_eq!(names, &["Noto Sans KR", "Noto Sans KR Medium"]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_name_table_full_of_names_is_capped() {
        let texts: Vec<String> = (0..100).map(|i| format!("Family {i}")).collect();
        let records: Vec<(u16, u16, &str)> = texts.iter().enumerate().map(|(i, t)| (0x0400 + i as u16, 1, t.as_str())).collect();
        let (_, _, names, _, _, _) = &file_faces(&font_with_names(&records))[0];
        assert_eq!(names.len(), MAX_FAMILY_NAMES);
    }

    #[test]
    fn a_family_is_found_by_any_of_its_names() {
        let names = ["Malgun Gothic", "맑은 고딕"];
        let cat = [face("Malgun Gothic", &names, "malgunbd.ttf", 700.0), face("Malgun Gothic", &names, "malgun.ttf", 400.0)];
        for wanted in ["Malgun Gothic", "malgun gothic", "맑은 고딕"] {
            let found: Vec<f32> = faces_named(&cat, wanted).iter().map(|f| f.weight).collect();
            assert_eq!(found, [400.0, 700.0], "{wanted:?}: both faces, regular first");
        }
        assert!(faces_named(&cat, "맑은").is_empty(), "a part of a name is not a name");
        assert!(faces_named(&cat, "").is_empty());
    }

    #[test]
    fn a_name_two_families_claim_has_a_fixed_owner() {
        let a = face("Alpha", &["Alpha", "공유"], "a/alpha.ttf", 400.0);
        let b = face("Beta", &["Beta", "공유"], "b/beta.ttf", 400.0);
        // Whatever order the scan saw the files in, the first by path wins.
        for cat in [[a.clone(), b.clone()], [b.clone(), a.clone()]] {
            let found = faces_named(&cat, "공유");
            assert_eq!(found.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(), ["Alpha"]);
        }
        // A family's own name beats another family's alias, even one scanned earlier.
        let c = face("Gamma", &["Gamma", "Beta"], "0/gamma.ttf", 400.0);
        let found = faces_named(&[c, b], "Beta");
        assert_eq!(found.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(), ["Beta"]);
    }

    #[test]
    fn a_legacy_name_picks_its_own_face_out_of_the_family() {
        let cat = [
            face("Noto Sans KR", &["Noto Sans KR"], "NotoSansKR-Regular.otf", 400.0),
            face("Noto Sans KR", &["Noto Sans KR", "Noto Sans KR Medium"], "NotoSansKR-Medium.otf", 500.0),
        ];
        assert_eq!(faces_named(&cat, "Noto Sans KR").len(), 2);
        let medium = faces_named(&cat, "Noto Sans KR Medium");
        assert_eq!(medium.iter().map(|f| f.weight).collect::<Vec<_>>(), [500.0]);
    }

    #[test]
    fn installed_names_are_kept_as_the_workbook_spells_them() {
        // The binding is keyed on the name `cell_family` looks up, so every spelling in use is
        // returned as written: the English and the Korean name both bind, over the same faces.
        let cat = [face("Malgun Gothic", &["Malgun Gothic", "맑은 고딕"], "malgun.ttf", 400.0)];
        let used: Vec<String> = ["맑은 고딕", "Malgun Gothic", "Not Installed", "맑은 고딕", "malgun gothic"].map(String::from).to_vec();
        assert_eq!(installed_in(&cat, &used), ["Malgun Gothic", "malgun gothic", "맑은 고딕"]);
        assert!(installed_in(&[], &used).is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_family_bound_under_two_names_shares_its_font_data() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Ubuntu-Light.ttf");
        std::fs::write(&path, epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        let f = Face {
            family: "Ubuntu".into(),
            names: vec!["Ubuntu".into(), "우분투".into()],
            path,
            index: 0,
            weight: 300.0,
            italic: false,
            serif: false,
            scripts: ScriptSet::of_text("A"),
        };

        let mut fonts = egui::FontDefinitions::default();
        let before = fonts.font_data.len();
        let mut bound = bind_faces(&mut fonts, "Ubuntu", std::slice::from_ref(&f), &[], &[]);
        bound.extend(bind_faces(&mut fonts, "우분투", std::slice::from_ref(&f), &[], &[]));
        assert!(bound.contains(&role_key("우분투", false, false)), "bound under the workbook's name");
        assert!(bound.contains(&role_key("Ubuntu", true, true)));
        assert_eq!(fonts.font_data.len(), before + 1, "one file, loaded once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On a Windows machine with Malgun Gothic, the real file carries its Korean name.
    #[cfg(windows)]
    #[test]
    fn malgun_gothic_answers_to_its_korean_name_when_installed() {
        let root = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        let Ok(data) = std::fs::read(root.join("Fonts").join("malgun.ttf")) else { return };
        let faces = file_faces(&data);
        assert!(faces.iter().any(|(_, family, names, _, _, _)| family == "Malgun Gothic" && names.iter().any(|n| n == "맑은 고딕")), "{faces:?}");
    }
}
