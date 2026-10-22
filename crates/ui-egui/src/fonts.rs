//! The fonts installed on the machine, so the font list can offer them and cells can render with
//! them (#4).
//!
//! egui has no font discovery of its own and panics when asked for a family that was never bound
//! to font data. So we do both halves: discover installed faces for family names (a read of each
//! font file's `name` table — damaged or unreadable files yield nothing, never a panic), and bind
//! the files a workbook actually uses into the families
//! [`theme::font_definitions`](crate::theme::font_definitions) assembles.
//!
//! On Linux the file list comes from fontconfig (`fc-list`), which is part of the standard Linux
//! font stack and knows the distribution's font directories. Other platforms scan their known font
//! folders.
//!
//! Native only: on wasm there are no system fonts, every function here reports "nothing", and the
//! font list is the standard [`crate::ribbon::FONTS`] set.

use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::io::{Read, Seek, SeekFrom};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

/// TTC collections are allowed at most this many faces. The limit keeps a damaged header from
/// making the reader walk an unbounded array; real collections are far smaller.
#[cfg(not(target_arch = "wasm32"))]
const MAX_TTC_FACES: u32 = 1024;

/// SFNT table directories are allowed at most this many records. The format's field is `u16`, but
/// every real font is far below this bound, and a smaller ceiling keeps a damaged table directory
/// from allocating a large buffer.
#[cfg(not(target_arch = "wasm32"))]
const MAX_TABLE_RECORDS: usize = 4096;

/// A `name` table may contain at most this many records. Again, this is a format sanity bound,
/// not a font-file size limit.
#[cfg(not(target_arch = "wasm32"))]
const MAX_NAME_RECORDS: usize = 4096;

/// A single family or style name longer than this is treated as damaged. Real names are short.
#[cfg(not(target_arch = "wasm32"))]
const MAX_NAME_BYTES: usize = 8192;

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
}

/// The platform's font folders, used as a fallback when fontconfig is unavailable. Empty on wasm.
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
    let s: String = style.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect();
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
    let s: String = style.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect();
    s.contains("italic") || s.contains("oblique")
}

/// One name record from an SFNT `name` table.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
struct NameRecord {
    platform_id: u16,
    encoding_id: u16,
    language_id: u16,
    name_id: u16,
    length: u16,
    offset: u16,
}

/// Read exactly `length` bytes at `offset`, without loading the rest of the file.
#[cfg(not(target_arch = "wasm32"))]
fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, length: usize) -> Option<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut data = vec![0u8; length];
    reader.read_exact(&mut data).ok()?;
    Some(data)
}

#[cfg(not(target_arch = "wasm32"))]
fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(offset..offset.checked_add(2)?)?.try_into().ok()?))
}

#[cfg(not(target_arch = "wasm32"))]
fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(offset..offset.checked_add(4)?)?.try_into().ok()?))
}

/// How suitable a `name` record is: English Windows Unicode first, then other Windows Unicode,
/// other Windows encodings, Unicode, Macintosh, and finally whatever remains.
#[cfg(not(target_arch = "wasm32"))]
fn name_rank(record: &NameRecord) -> u8 {
    if record.platform_id == 3 && (record.encoding_id == 1 || record.encoding_id == 10) && record.language_id == 0x0409 {
        0
    } else if record.platform_id == 3 && (record.encoding_id == 1 || record.encoding_id == 10) {
        1
    } else if record.platform_id == 3 {
        2
    } else if record.platform_id == 0 {
        3
    } else if record.platform_id == 1 {
        4
    } else {
        5
    }
}

/// Decode one name record. Windows and Unicode records are UTF-16BE; Macintosh Roman is treated as
/// Latin-1, which covers the ASCII family names that matter for discovery.
#[cfg(not(target_arch = "wasm32"))]
fn decode_name<R: Read + Seek>(reader: &mut R, string_base: u64, string_limit: u64, record: &NameRecord) -> Option<String> {
    let length = record.length as usize;
    if length == 0 || length > MAX_NAME_BYTES {
        return None;
    }
    let start = string_base.checked_add(record.offset as u64)?;
    let end = start.checked_add(length as u64)?;
    if end > string_limit {
        return None;
    }
    let bytes = read_at(reader, start, length)?;
    let text = match record.platform_id {
        0 | 3 => {
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes.chunks_exact(2).map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]])).collect();
            String::from_utf16(&units).ok()?
        }
        1 => bytes.iter().map(|byte| *byte as char).collect(),
        _ => String::from_utf8(bytes).ok()?,
    };
    let text = text.trim_matches('\0').trim();
    if text.is_empty() { None } else { Some(text.to_string()) }
}

/// Pick the best decodable name for the first matching ID in `ids`, trying IDs in priority order.
#[cfg(not(target_arch = "wasm32"))]
fn select_name<R: Read + Seek>(reader: &mut R, string_base: u64, string_limit: u64, records: &[NameRecord], ids: &[u16]) -> Option<String> {
    for id in ids {
        let mut candidates: Vec<(u8, usize)> =
            records.iter().enumerate().filter(|(_, record)| record.name_id == *id).map(|(index, record)| (name_rank(record), index)).collect();
        candidates.sort_by_key(|(rank, index)| (*rank, *index));
        for (_, index) in candidates {
            if let Some(name) = decode_name(reader, string_base, string_limit, records.get(index)?) {
                return Some(name);
            }
        }
    }
    None
}

/// Read family and style from one face's `name` table.
#[cfg(not(target_arch = "wasm32"))]
fn read_name_table<R: Read + Seek>(reader: &mut R, table_offset: u64, table_len: u32) -> Option<(String, String)> {
    let table_len = table_len as usize;
    if table_len < 6 {
        return None;
    }
    let header = read_at(reader, table_offset, 6)?;
    let format = u16_at(&header, 0)?;
    if format > 1 {
        return None;
    }
    let count = u16_at(&header, 2)? as usize;
    if count == 0 || count > MAX_NAME_RECORDS {
        return None;
    }
    let string_offset = u16_at(&header, 4)? as u64;
    let records_len = count.checked_mul(12)?;
    if 6usize.checked_add(records_len)? > table_len {
        return None;
    }
    let records_data = read_at(reader, table_offset.checked_add(6)?, records_len)?;
    let string_base = table_offset.checked_add(string_offset)?;
    let table_end = table_offset.checked_add(table_len as u64)?;
    if string_base > table_end {
        return None;
    }
    let mut records: Vec<NameRecord> = Vec::with_capacity(count);
    for index in 0..count {
        let record = records_data.get(index.checked_mul(12)?..index.checked_mul(12)?.checked_add(12)?)?;
        records.push(NameRecord {
            platform_id: u16_at(record, 0)?,
            encoding_id: u16_at(record, 2)?,
            language_id: u16_at(record, 4)?,
            name_id: u16_at(record, 6)?,
            length: u16_at(record, 8)?,
            offset: u16_at(record, 10)?,
        });
    }
    let family = select_name(reader, string_base, table_end, &records, &[16, 1])?;
    let style = select_name(reader, string_base, table_end, &records, &[17, 2]).unwrap_or_else(|| "Regular".into());
    Some((family, style))
}

/// Read the OS/2 panose byte pair and say whether it declares a serif style: latin-text family
/// (2) with a serif style in 2..=10. A missing or short table is treated as sans, the old
/// behaviour for every face.
#[cfg(not(target_arch = "wasm32"))]
fn read_os2_serif<R: Read + Seek>(reader: &mut R, file_len: u64, table_offset: u64, table_len: u32) -> bool {
    const PANOSE_OFFSET: u64 = 32;
    const PANOSE_END: u64 = 42;
    if table_len < PANOSE_END as u32 {
        return false;
    }
    let Some(end) = table_offset.checked_add(PANOSE_END) else { return false };
    if end > file_len {
        return false;
    }
    let Some(panose) = read_at(reader, table_offset + PANOSE_OFFSET, (PANOSE_END - PANOSE_OFFSET) as usize) else {
        return false;
    };
    panose.first() == Some(&2) && matches!(panose.get(1).copied(), Some(2..=10))
}

/// Read family, style and serif flag from the face whose SFNT offset table starts at `face_offset`.
#[cfg(not(target_arch = "wasm32"))]
fn face_names<R: Read + Seek>(reader: &mut R, file_len: u64, face_offset: u64) -> Option<(String, String, bool)> {
    if face_offset.checked_add(12)? > file_len {
        return None;
    }
    let header = read_at(reader, face_offset, 12)?;
    let version = u32_at(&header, 0)?;
    if !matches!(version, 0x0001_0000 | 0x4F54_544F | 0x7472_7565 | 0x7479_7031) {
        return None;
    }
    let num_tables = u16_at(&header, 4)? as usize;
    if num_tables == 0 || num_tables > MAX_TABLE_RECORDS {
        return None;
    }
    let directory_len = 12usize.checked_add(num_tables.checked_mul(16)?)?;
    if face_offset.checked_add(directory_len as u64)? > file_len {
        return None;
    }
    let directory = read_at(reader, face_offset, directory_len)?;
    let mut name_table: Option<(u64, u32)> = None;
    let mut serif = false;
    for index in 0..num_tables {
        let start = 12usize.checked_add(index.checked_mul(16)?)?;
        let record = directory.get(start..start.checked_add(16)?)?;
        let tag = record.get(0..4)?;
        let table_offset = u32_at(record, 8)? as u64;
        let table_len = u32_at(record, 12)?;
        if table_offset.checked_add(table_len as u64)? > file_len {
            continue;
        }
        if tag == b"name".as_slice() {
            name_table = Some((table_offset, table_len));
        } else if tag == b"OS/2".as_slice() {
            serif = read_os2_serif(reader, file_len, table_offset, table_len);
        }
    }
    let (table_offset, table_len) = name_table?;
    let (family, style) = read_name_table(reader, table_offset, table_len)?;
    Some((family, style, serif))
}

/// Every face in `reader` as `(index, family, style, serif)`. Damaged data yields nothing, never a
/// panic.
#[cfg(not(target_arch = "wasm32"))]
fn faces_in<R: Read + Seek>(reader: &mut R, file_len: u64) -> Vec<(u32, String, String, bool)> {
    let mut out: Vec<(u32, String, String, bool)> = Vec::new();
    if file_len < 12 {
        return out;
    }
    let Some(header) = read_at(reader, 0, 12) else { return out };
    if u32_at(&header, 0) == Some(0x7474_6366) {
        let version = u32_at(&header, 4).unwrap_or(0);
        if version != 0x0001_0000 && version != 0x0002_0000 {
            return out;
        }
        let num_fonts = u32_at(&header, 8).unwrap_or(0);
        if num_fonts == 0 || num_fonts > MAX_TTC_FACES {
            return out;
        }
        let Some(offsets_len) = (num_fonts as usize).checked_mul(4) else { return out };
        let Some(offsets) = read_at(reader, 12, offsets_len) else { return out };
        for index in 0..num_fonts {
            let Some(offset) = u32_at(&offsets, index as usize * 4) else { continue };
            if let Some((family, style, serif)) = face_names(reader, file_len, offset as u64) {
                out.push((index, family, style, serif));
            }
        }
        return out;
    }
    if let Some((family, style, serif)) = face_names(reader, file_len, 0) {
        out.push((0, family, style, serif));
    }
    out
}

/// Every face in `path` as `(index, family, style, serif)`, read from the file's metadata only.
#[cfg(not(target_arch = "wasm32"))]
fn file_faces(path: &Path) -> Vec<(u32, String, String, bool)> {
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(file_len) = file.metadata().map(|metadata| metadata.len()) else { return Vec::new() };
    faces_in(&mut file, file_len)
}

/// Whether `path` names a font format the scanner understands.
#[cfg(not(target_arch = "wasm32"))]
fn is_font_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc"))
}

/// Every face in `path` appended to `out`. Unsupported, unreadable or damaged files contribute
/// nothing. Only the table directory and `name` table are read; the font data itself is loaded
/// when a face is bound.
#[cfg(not(target_arch = "wasm32"))]
fn scan_file(path: &Path, out: &mut Vec<Face>) {
    if !is_font_path(path) {
        return;
    }
    for (index, family, style, serif) in file_faces(path) {
        out.push(Face { family, path: path.to_path_buf(), index, weight: style_weight(&style), italic: style_italic(&style), serif });
    }
}

/// Catalog every face in `files`, reading each path once. Only the Linux catalog and tests use
/// this; other targets discover files by walking their font folders.
#[cfg(any(target_os = "linux", all(test, not(target_arch = "wasm32"))))]
fn scan_files(files: &[PathBuf]) -> Vec<Face> {
    let mut out: Vec<Face> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for path in files {
        if seen.insert(path.clone()) {
            scan_file(path, &mut out);
        }
    }
    out
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

/// Font files known to fontconfig, in the order it reports them. `fc-list` is the usual Linux
/// font source and covers distribution-specific directories that [`system_font_dirs`] cannot
/// know.
#[cfg(target_os = "linux")]
fn fontconfig_files() -> Vec<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let Ok(output) = std::process::Command::new("fc-list").args(["-f", "%{file}\n"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for line in output.stdout.split(|byte| *byte == b'\n') {
        // Keep paths byte-exact: a font path need not be valid UTF-8.
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let path = PathBuf::from(OsStr::from_bytes(line));
        if path.is_file() && is_font_path(&path) && seen.insert(path.clone()) {
            files.push(path);
        }
    }
    files
}

#[cfg(target_arch = "wasm32")]
pub fn scan_dirs(_dirs: &[PathBuf]) -> Vec<Face> {
    Vec::new()
}

/// The installed faces, preferring fontconfig's list on Linux and falling back to the platform's
/// font folders when fontconfig is unavailable or empty.
fn scan_installed() -> Vec<Face> {
    #[cfg(target_os = "linux")]
    {
        let files = fontconfig_files();
        if !files.is_empty() {
            let faces = scan_files(&files);
            if !faces.is_empty() {
                return faces;
            }
        }
    }
    scan_dirs(&system_font_dirs())
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
fn pick_face(faces: &[Face], bold: bool, italic: bool) -> Option<&Face> {
    let regular = || faces.iter().find(|f| !f.italic && (f.weight - 400.0).abs() < 1.0);
    let exact = |wanted_bold: bool, wanted_italic: bool| {
        faces.iter().find(|f| f.italic == wanted_italic && if wanted_bold { f.weight >= 600.0 } else { f.weight < 600.0 })
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

/// The script fallback keys a bound family carries: `sans` for upright faces and `serif` for faces
/// whose panose says serif. Keys must already have data in `fonts.font_data` (see
/// `theme::font_definitions_with_bound`).
#[derive(Clone, Debug, Default)]
pub struct ScriptFallbacks {
    pub sans: Vec<String>,
    pub serif: Vec<String>,
}

/// Bind one installed family's faces into `fonts`, one egui family per cell role, and return the
/// role keys that are now bound. A role with no exact face chains to the family's regular face, so
/// no role is ever left pointing at missing data (egui panics on that). A family with no faces
/// binds nothing.
///
/// Each role's chain ends with the script fallbacks for its face (`fallback.serif` for a serif
/// face, otherwise `fallback.sans`), then egui's default proportional fonts. The script keys must
/// be explicit: at this point `FontFamily::Proportional` may still be egui's bundled fonts, which
/// cover Latin and emoji only — a Latin-only family such as DejaVu Sans would otherwise show tofu
/// for CJK (#1231).
pub fn bind_faces(fonts: &mut egui::FontDefinitions, family: &str, faces: &[Face], fallback: &ScriptFallbacks) -> Vec<String> {
    if faces.is_empty() {
        return Vec::new();
    }
    let base: Vec<String> = fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
    let mut bound: Vec<String> = Vec::new();
    for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
        let Some(primary) = pick_face(faces, bold, italic) else { continue };
        let script = if primary.serif { &fallback.serif } else { &fallback.sans };
        let mut chain: Vec<String> = Vec::new();
        for face in [Some(primary), pick_face(faces, false, false)] {
            let Some(face) = face else { continue };
            let key = face_key(face);
            if chain.contains(&key) {
                continue;
            }
            if fonts.font_data.contains_key(&key) {
                chain.push(key);
                continue;
            }
            let Ok(bytes) = std::fs::read(&face.path) else { continue };
            let mut fd = egui::FontData::from_owned(bytes);
            fd.index = face.index;
            fonts.font_data.insert(key.clone(), std::sync::Arc::new(fd));
            chain.push(key);
        }
        if chain.is_empty() {
            continue;
        }
        chain.extend(script.iter().cloned());
        chain.extend(base.iter().cloned());
        // A key without data would make egui panic when it builds the atlas.
        chain.retain(|k| fonts.font_data.contains_key(k));
        let role = role_key(family, bold, italic);
        fonts.families.insert(egui::FontFamily::Name(role.clone().into()), chain);
        bound.push(role);
    }
    bound
}

/// Bind every installed face of `family` (see [`bind_faces`]).
pub fn bind_family(fonts: &mut egui::FontDefinitions, family: &str, fallback: &ScriptFallbacks) -> Vec<String> {
    bind_faces(fonts, family, &faces_of(family), fallback)
}

/// The role families currently bound to font data. `cell_family` consults this, so it never names a
/// family egui hasn't been given.
static BOUND: OnceLock<std::sync::RwLock<HashSet<String>>> = OnceLock::new();

fn bound() -> &'static std::sync::RwLock<HashSet<String>> {
    BOUND.get_or_init(|| std::sync::RwLock::new(HashSet::new()))
}

/// Record the role families a freshly built [`egui::FontDefinitions`] bound.
pub fn set_bound(keys: impl IntoIterator<Item = String>) {
    *bound().write().unwrap_or_else(|e| e.into_inner()) = keys.into_iter().collect();
}

/// Is this exact role family bound to font data?
pub fn is_bound(role: &str) -> bool {
    bound().read().unwrap_or_else(|e| e.into_inner()).contains(role)
}

/// Serializes tests that mutate the process-wide bound-family registry.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn damaged_or_non_font_files_contribute_nothing() {
        let damaged = b"ttcf\x00\x01\x00\x00\xff\xff\xff\xff";
        assert!(faces_in(&mut std::io::Cursor::new(damaged), damaged.len() as u64).is_empty());
        let text = b"not a font at all";
        assert!(faces_in(&mut std::io::Cursor::new(text), text.len() as u64).is_empty());
        assert!(faces_in(&mut std::io::Cursor::new(Vec::new()), 0).is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn faces_in_reads_ttf_name_table() {
        let font = epaint_default_fonts::UBUNTU_LIGHT;
        let faces = faces_in(&mut std::io::Cursor::new(font), font.len() as u64);
        assert_eq!(faces, vec![(0, "Ubuntu".into(), "Light".into(), false)]);
    }

    /// Wrap one TTF twice in a TTC. Table offsets in a collection are relative to the TTC start,
    /// so each copied face's table records have to be shifted by the face's base offset.
    #[cfg(not(target_arch = "wasm32"))]
    fn ttc_with_two_faces(font: &[u8]) -> Vec<u8> {
        let num_tables = u16::from_be_bytes([font[4], font[5]]) as usize;
        let face_len = u32::try_from(font.len()).expect("font fits u32");
        let first = 12u32 + 4 * 2;
        let second = first + face_len;
        let mut ttc = Vec::with_capacity((second + face_len) as usize);
        ttc.extend_from_slice(b"ttcf");
        ttc.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        ttc.extend_from_slice(&2u32.to_be_bytes());
        ttc.extend_from_slice(&first.to_be_bytes());
        ttc.extend_from_slice(&second.to_be_bytes());
        ttc.extend_from_slice(font);
        ttc.extend_from_slice(font);
        for base in [first, second] {
            for index in 0..num_tables {
                let at = base as usize + 12 + index * 16 + 8;
                let old = u32::from_be_bytes(ttc[at..at + 4].try_into().expect("table offset"));
                ttc[at..at + 4].copy_from_slice(&old.checked_add(base).expect("shifted offset").to_be_bytes());
            }
        }
        ttc
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn faces_in_reads_every_ttc_face() {
        let ttc = ttc_with_two_faces(epaint_default_fonts::UBUNTU_LIGHT);
        let faces = faces_in(&mut std::io::Cursor::new(&ttc), ttc.len() as u64);
        assert_eq!(faces.len(), 2);
        assert_eq!(faces[0], (0, "Ubuntu".into(), "Light".into(), false));
        assert_eq!(faces[1], (1, "Ubuntu".into(), "Light".into(), false));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn faces_in_rejects_out_of_bounds_tables() {
        let mut font = epaint_default_fonts::UBUNTU_LIGHT.to_vec();
        let num_tables = u16::from_be_bytes([font[4], font[5]]) as usize;
        for index in 0..num_tables {
            let at = 12 + index * 16;
            if &font[at..at + 4] == b"name" {
                font[at + 8..at + 12].copy_from_slice(&u32::MAX.to_be_bytes());
            }
        }
        assert!(faces_in(&mut std::io::Cursor::new(&font), font.len() as u64).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn scan_dirs_reads_large_fonts_without_a_size_cap() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-large-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("large.ttf");
        std::fs::write(&path, epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        let file = std::fs::OpenOptions::new().write(true).open(&path).expect("open");
        file.set_len((64u64 << 20) + 1).expect("extend");
        let faces = scan_dirs(std::slice::from_ref(&dir));
        assert_eq!(faces.len(), 1, "the old 64 MiB cap no longer skips the font");
        assert_eq!(faces[0].family, "Ubuntu");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fontconfig_files_are_existing_supported_fonts() {
        let files = fontconfig_files();
        // A minimal container may not have fontconfig or any fonts; the folder scan is the
        // fallback in that case. When fc-list does answer, every path must be usable.
        if files.is_empty() {
            return;
        }
        assert!(files.iter().all(|path| path.is_file() && is_font_path(path)));
        let unique: HashSet<&PathBuf> = files.iter().collect();
        assert_eq!(unique.len(), files.len(), "fc-list duplicates are removed");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn scan_files_catalogs_supported_fonts_once() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let ubuntu = dir.join("Ubuntu-Light.ttf");
        std::fs::write(&ubuntu, epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        std::fs::write(dir.join("damaged.ttf"), b"ttcf\x00\x01\x00\x00\xff\xff\xff\xff").expect("write");
        std::fs::write(dir.join("readme.txt"), b"not a font").expect("write");

        // The same path twice is read once; damaged and non-font files contribute nothing.
        let faces = scan_files(&[ubuntu.clone(), dir.join("damaged.ttf"), dir.join("readme.txt"), ubuntu]);
        assert_eq!(faces.len(), 1, "one face from one supported, readable file");
        assert_eq!(faces[0].family, "Ubuntu");
        assert_eq!(faces[0].weight, 300.0);
        let _ = std::fs::remove_dir_all(&dir);
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
        let face = Face { family: "Ubuntu".into(), names: vec!["Ubuntu".into()], path, index: 0, weight: 300.0, italic: false, serif: false };

        let mut fonts = egui::FontDefinitions::default();
        let fallback = ScriptFallbacks::default();
        let bound = bind_faces(&mut fonts, "Ubuntu", std::slice::from_ref(&face), &fallback);
        assert_eq!(bound.len(), 4, "regular, bold, italic and bold-italic roles");
        for key in &bound {
            let chain = fonts.families.get(&egui::FontFamily::Name(key.as_str().into())).expect("role family");
            assert!(chain.iter().all(|k| fonts.font_data.contains_key(k)), "every chained key has data");
        }
        // A family with no faces binds nothing and never touches the map.
        assert!(bind_faces(&mut fonts, "No Such Font", &[], &fallback).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Latin-only family such as DejaVu Sans must keep the CJK/Thai/... fallback keys in its
    /// chain; without them a Chinese cell in DejaVu Sans shows tofu (#1231).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_bound_family_carries_its_sans_script_fallbacks() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-sans-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Ubuntu-Light.ttf");
        std::fs::write(&path, epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        let face = Face { family: "Ubuntu".into(), path, index: 0, weight: 300.0, italic: false, serif: false };

        let mut fonts = egui::FontDefinitions::default();
        let data = || std::sync::Arc::new(egui::FontData::from_owned(epaint_default_fonts::UBUNTU_LIGHT.to_vec()));
        fonts.font_data.insert("sys-cjk-han-zh".into(), data());
        fonts.font_data.insert("sys-cjk-han-zh-serif".into(), data());
        let fallback = ScriptFallbacks { sans: vec!["sys-cjk-han-zh".into()], serif: vec!["sys-cjk-han-zh-serif".into()] };
        let bound = bind_faces(&mut fonts, "Ubuntu", std::slice::from_ref(&face), &fallback);
        for key in &bound {
            let chain = fonts.families.get(&egui::FontFamily::Name(key.as_str().into())).expect("role family");
            assert!(chain.contains(&"sys-cjk-han-zh".to_string()), "sans face keeps the sans fallback");
            assert!(!chain.contains(&"sys-cjk-han-zh-serif".to_string()), "sans face does not pull the serif fallback");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_serif_face_uses_the_serif_fallbacks() {
        let dir = std::env::temp_dir().join(format!("gridcraft-fonts-serif-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Ubuntu-Light.ttf");
        std::fs::write(&path, epaint_default_fonts::UBUNTU_LIGHT).expect("write");
        let face = Face { family: "Ubuntu".into(), path, index: 0, weight: 300.0, italic: false, serif: true };

        let mut fonts = egui::FontDefinitions::default();
        let data = || std::sync::Arc::new(egui::FontData::from_owned(epaint_default_fonts::UBUNTU_LIGHT.to_vec()));
        fonts.font_data.insert("sys-cjk-han-zh".into(), data());
        fonts.font_data.insert("sys-cjk-han-zh-serif".into(), data());
        let fallback = ScriptFallbacks { sans: vec!["sys-cjk-han-zh".into()], serif: vec!["sys-cjk-han-zh-serif".into()] };
        let bound = bind_faces(&mut fonts, "Ubuntu", std::slice::from_ref(&face), &fallback);
        for key in &bound {
            let chain = fonts.families.get(&egui::FontFamily::Name(key.as_str().into())).expect("role family");
            assert!(chain.contains(&"sys-cjk-han-zh-serif".to_string()), "serif face keeps the serif fallback");
            assert!(!chain.contains(&"sys-cjk-han-zh".to_string()), "serif face does not pull the sans fallback");
        }
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

    fn face(family: &str, names: &[&str], path: &str, weight: f32) -> Face {
        Face { family: family.into(), names: names.iter().map(|s| s.to_string()).collect(), path: path.into(), index: 0, weight, italic: false }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_face_keeps_its_family_names_in_every_language() {
        // Korean Excel saves `맑은 고딕`, so the Korean name has to be kept next to the English one (#1898).
        let font = font_with_names(&[(EN, 1, "Malgun Gothic"), (EN, 2, "Regular"), (KO, 1, "맑은 고딕"), (KO, 2, "보통")]);
        let faces = file_faces(&font);
        assert_eq!(faces.len(), 1);
        let (_, family, names, style) = &faces[0];
        assert_eq!(family, "Malgun Gothic", "the font list keeps the English name");
        assert_eq!(names, &["Malgun Gothic", "맑은 고딕"]);
        assert_eq!(style, "Regular");

        // Both name IDs are kept: a legacy family name (ID 1) names a single face (#1606).
        let font = font_with_names(&[(EN, 1, "Noto Sans KR Medium"), (EN, 2, "Regular"), (EN, 16, "Noto Sans KR"), (EN, 17, "Medium")]);
        let (_, family, names, _) = &file_faces(&font)[0];
        assert_eq!(family, "Noto Sans KR");
        assert_eq!(names, &["Noto Sans KR", "Noto Sans KR Medium"]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_name_table_full_of_names_is_capped() {
        let texts: Vec<String> = (0..100).map(|i| format!("Family {i}")).collect();
        let records: Vec<(u16, u16, &str)> = texts.iter().enumerate().map(|(i, t)| (0x0400 + i as u16, 1, t.as_str())).collect();
        let (_, _, names, _) = &file_faces(&font_with_names(&records))[0];
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
        let f = Face { family: "Ubuntu".into(), names: vec!["Ubuntu".into(), "우분투".into()], path, index: 0, weight: 300.0, italic: false };

        let mut fonts = egui::FontDefinitions::default();
        let before = fonts.font_data.len();
        let mut bound = bind_faces(&mut fonts, "Ubuntu", std::slice::from_ref(&f));
        bound.extend(bind_faces(&mut fonts, "우분투", std::slice::from_ref(&f)));
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
        assert!(faces.iter().any(|(_, family, names, _)| family == "Malgun Gothic" && names.iter().any(|n| n == "맑은 고딕")), "{faces:?}");
    }
}
