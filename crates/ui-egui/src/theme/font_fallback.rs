//! The non-Latin faces every family carries as its last resort, and the loader that puts them in
//! the font set. Split out of `theme` so the lists stay next to the code that chooses between them.

#[cfg(not(target_arch = "wasm32"))]
use super::Arc;
use super::FontDefinitions;
use super::font_setup::HanOrder;
#[cfg(not(target_arch = "wasm32"))]
use super::font_setup::try_load;
use crate::script::{ScriptClass, ScriptSet};

/// Every face `fc-match` answers for the given patterns, in order.
#[cfg(target_os = "linux")]
fn fc_match_candidates(patterns: &[&str]) -> Vec<(String, u32)> {
    let mut candidates = Vec::new();
    for pattern in patterns {
        if let Some(font) = crate::fonts::fc_match(pattern)
            && !candidates.contains(&font)
        {
            candidates.push(font);
        }
    }
    candidates
}

/// The system's colour emoji face.
///
/// `emoji` is the generic family fontconfig itself ranks emoji fonts by: `45-generic.conf` aliases
/// every colour emoji face to it and `60-generic.conf` prefers the colour ones over the monochrome
/// Noto Emoji. Asking for the family therefore follows the distribution's policy instead of
/// hard-coding a vendor path, and it keeps working inside a Flatpak, where the host's fontconfig
/// answers for `/run/host/fonts`.
///
/// `★`, `✓` and `→` are not in a colour emoji face, so they keep falling through to [`symbols`].
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn emoji() -> Vec<(String, u32)> {
    #[cfg(target_os = "linux")]
    let faces = fc_match_candidates(&["emoji", "Noto Color Emoji"]);
    #[cfg(target_os = "macos")]
    let faces = vec![("/System/Library/Fonts/Apple Color Emoji.ttc".into(), 0)];
    #[cfg(target_os = "windows")]
    let faces = {
        let win = std::env::var("WINDIR").map_or_else(|_| "C:\\Windows\\Fonts".to_string(), |w| format!("{w}\\Fonts"));
        vec![(format!("{win}\\seguiemj.ttf"), 0)]
    };
    faces
}

/// Whether the system has a colour emoji face (tests skip their glyph checks without one).
#[cfg(test)]
pub fn colour_emoji_installed() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        emoji().iter().any(|(path, _)| std::path::Path::new(path).exists())
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

pub(super) struct ScriptFace {
    pub(super) key: &'static str,
    sans: Vec<(String, u32)>,
    serif: Option<(&'static str, Vec<(String, u32)>)>,
    /// The scripts this face exists for: it is loaded only when the text or the interface needs one.
    covers: ScriptSet,
}

/// The scripts the interface itself draws: a Chinese, Japanese or Korean interface needs its
/// script loaded even when the workbook holds only Latin text.
fn interface_scripts(language: crate::i18n::Language) -> ScriptSet {
    match language {
        crate::i18n::Language::Zh => ScriptSet::EMPTY.with(ScriptClass::Han),
        crate::i18n::Language::Ja => ScriptSet::EMPTY.with(ScriptClass::Han).with(ScriptClass::Japanese),
        crate::i18n::Language::Ko => ScriptSet::EMPTY.with(ScriptClass::Korean),
        _ => ScriptSet::EMPTY,
    }
}

/// The scripts whose fallback faces to load: the workbook's, the interface's and the symbols every
/// interface draws. An empty workbook set (no text yet, or nothing scanned) keeps the old
/// behaviour and loads every face.
pub(super) fn wanted_scripts(language: crate::i18n::Language, scripts: ScriptSet) -> ScriptSet {
    if scripts.is_empty() {
        return ScriptSet::all();
    }
    scripts.union(interface_scripts(language)).with(ScriptClass::Symbols)
}

/// Loads the system non-Latin fallback faces (one file per script family) and returns their keys so
/// every family can carry them as fallback. egui's bundled fonts have no coverage beyond Latin, so
/// without this a Thai, Hebrew, Japanese or Chinese workbook shows tofu boxes even though the text
/// was read correctly. With a face installed those scripts get real glyphs instead of tofu; nothing
/// is bundled, and a machine without the face keeps the old behaviour.
///
/// `serif` loads the serif companion faces instead of the sans ones. The caller loads those only
/// when a workbook or role actually asks for a serif face: Noto Serif CJK alone is ~55 MB, so
/// paying for it on startup would be wrong for the common case.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn load_fallback(fonts: &mut FontDefinitions, han: HanOrder, serif: bool, wanted: ScriptSet, known: bool) -> Vec<String> {
    let mut keys = Vec::new();
    // Files already loaded as a fallback face. A file serves at most one key: the region-merged
    // Noto Sans CJK .ttc found for Han also carries Hangul (and Tahoma carries both Thai and
    // Hebrew), so loading it again for a later script would only duplicate tens of MB in memory.
    let mut loaded: Vec<String> = Vec::new();
    // The scripts the faces above actually answered for. A script can be wanted and still have no
    // face: the chains name files by path, and a host may install the script under a family those
    // paths do not name (a different package layout, or only one weight).
    let mut covered = ScriptSet::EMPTY;
    for face in fallback_faces(han) {
        if !face.covers.intersects(wanted) {
            continue; // nothing in this workbook or interface asks for the script
        }
        let (key, cands) = if serif {
            match &face.serif {
                Some((key, cands)) => (*key, cands),
                None => continue,
            }
        } else {
            (face.key, &face.sans)
        };
        for (path, index) in cands {
            if loaded.contains(path) {
                break; // a face loaded for an earlier script already covers this one
            }
            if let Some(fd) = try_load(&[(path.as_str(), *index)]) {
                fonts.font_data.insert(key.to_string(), Arc::new(fd));
                keys.push(key.to_string());
                loaded.push(path.clone());
                covered = covered.union(face.covers);
                break;
            }
        }
    }
    if known {
        keys.extend(load_catalog_fallback(fonts, wanted, covered, serif, &mut loaded));
    }
    keys
}

/// Loads one face from the installed catalog for every wanted script the hand-written chains did
/// not cover, so a script the system can draw is drawn even when its file sits where the chains do
/// not look (a package that names the family differently, or installs only the regular weight).
/// The catalog's choice is a single face per script — a fallback face only has to have the glyph,
/// and a cell asking for bold gets the closest face epaint finds in the chain, exactly as the
/// hand-written fallbacks already behave. Returns the keys of the faces it loaded.
#[cfg(not(target_arch = "wasm32"))]
fn load_catalog_fallback(fonts: &mut FontDefinitions, wanted: ScriptSet, covered: ScriptSet, serif: bool, loaded: &mut Vec<String>) -> Vec<String> {
    let Some(catalog) = crate::catalog::FontCatalog::installed() else { return Vec::new() };
    let mut keys = Vec::new();
    for script in ScriptClass::ALL {
        // Latin is drawn by the interface font and by every bound workbook font; adding a Latin
        // fallback face would put another Latin font at the end of chains that already have one.
        if script == ScriptClass::Latin {
            continue;
        }
        let single = ScriptSet::EMPTY.with(script);
        if !wanted.intersects(single) || covered.intersects(single) {
            continue;
        }
        let Some(entry) = catalog.font_for_script(script, serif, false) else { continue };
        let path = entry.path.to_string_lossy().into_owned();
        // A file already in the font set serves every script it covers; loading it again under a
        // second key would duplicate its bytes in memory (a CJK collection is tens of megabytes).
        if loaded.contains(&path) {
            continue;
        }
        let Some(fd) = try_load(&[(path.as_str(), entry.index)]) else { continue };
        let key = format!("sys-script-{}{}", script.name(), if serif { "-serif" } else { "" });
        fonts.font_data.insert(key.clone(), Arc::new(fd));
        keys.push(key);
        loaded.push(path);
    }
    keys
}

/// Faces for the scripts the workbook sets in bold, when the host has them.
///
/// A Calibri cell takes its Latin from Carlito Bold, so the Han glyphs have to come from a bold CJK
/// face or half the cell stays regular. Resolving through the catalog keeps Flatpak honest: the
/// sandbox installs no CJK font, it sees the host's faces, and a host without a bold face simply
/// leaves the chain as it is (#1231).
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn load_bold_script_faces(fonts: &mut FontDefinitions, wanted_bold: ScriptSet, serif: bool) -> Vec<String> {
    let Some(catalog) = crate::catalog::FontCatalog::installed() else { return Vec::new() };
    let mut keys = Vec::new();
    let mut loaded: Vec<String> = Vec::new();
    for script in ScriptClass::ALL {
        // Latin comes from the bound family's own bold face (Carlito Bold, Liberation Sans Bold).
        if script == ScriptClass::Latin || !wanted_bold.intersects(ScriptSet::EMPTY.with(script)) {
            continue;
        }
        let Some(preferred) = catalog.font_for_script(script, serif, true) else { continue };
        if preferred.weight < 600.0 {
            continue; // no bold face for this script on this host
        }
        // The best ranked face may be one egui cannot read (a variable collection's bold is a
        // named instance), while the same family also ships a static bold file (#1231). Try the
        // other bold faces before giving up on this script.
        let others = catalog.entries().iter().filter(|entry| {
            entry.weight >= 600.0 && entry.serif == serif && entry.path != preferred.path && entry.scripts.intersects(ScriptSet::EMPTY.with(script))
        });
        let Some((entry, fd)) = std::iter::once(preferred).chain(others).find_map(|entry| {
            let path = entry.path.to_string_lossy().into_owned();
            if loaded.contains(&path) {
                return None; // one file, several scripts
            }
            try_load(&[(path.as_str(), entry.index)]).map(|fd| (entry, fd))
        }) else {
            continue;
        };
        let key = format!("sys-script-{}-bold{}", script.name(), if serif { "-serif" } else { "" });
        fonts.font_data.insert(key.clone(), Arc::new(fd));
        keys.push(key);
        loaded.push(entry.path.to_string_lossy().into_owned());
    }
    keys
}

#[cfg(target_arch = "wasm32")]
pub(super) fn load_fallback(_fonts: &mut FontDefinitions, _han: HanOrder, _serif: bool, _wanted: ScriptSet, _known: bool) -> Vec<String> {
    Vec::new()
}

/// The face fontconfig prefers for Arabic, such as a Vazirmatn the user set as their Arabic font
/// (#229), kept only when it has Arabic letters. Asked once per process.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn fontconfig_arabic() -> Option<(String, u32)> {
    static PREFERRED: std::sync::OnceLock<Option<(String, u32)>> = std::sync::OnceLock::new();
    PREFERRED
        .get_or_init(|| {
            let out = std::process::Command::new("fc-match").args(["--format=%{file}\n%{index}\n", ":lang=ar"]).output().ok()?;
            let (file, index) = parse_fc_match(&String::from_utf8_lossy(&out.stdout)).filter(|_| out.status.success())?;
            let bytes = crate::fonts::read_font(std::path::Path::new(&file))?;
            covers_arabic(&bytes, index).then_some((file, index))
        })
        .clone()
}

#[cfg(all(not(target_arch = "wasm32"), not(all(unix, not(target_os = "macos")))))]
pub(super) fn fontconfig_arabic() -> Option<(String, u32)> {
    None
}

/// The file and face index from `fc-match --format='%{file}\n%{index}\n'`. fontconfig packs a
/// variable font's named instance into the upper 16 bits of the index; only the face index is kept.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn parse_fc_match(out: &str) -> Option<(String, u32)> {
    let mut lines = out.lines().map(str::trim);
    let file = lines.next().filter(|file| !file.is_empty())?;
    let index = lines.next().and_then(|index| index.parse::<u32>().ok()).map_or(0, |packed| packed & 0xFFFF);
    Some((file.to_owned(), index))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn covers_arabic(bytes: &[u8], index: u32) -> bool {
    use skrifa::MetadataProvider;
    skrifa::FontRef::from_index(bytes, index).is_ok_and(|font| "العربية".chars().all(|ch| font.charmap().map(ch).is_some()))
}

#[cfg(target_arch = "wasm32")]
pub(super) fn load_bold_script_faces(_fonts: &mut FontDefinitions, _wanted_bold: ScriptSet, _serif: bool) -> Vec<String> {
    Vec::new()
}

/// Every fallback face we look for, in chain order, with its sans and (where it exists) serif
/// lists. One file per script family is enough — epaint picks the first face with the glyph, and a
/// script's glyphs live in one file. Order only matters between faces that both cover a glyph
/// (Japanese vs Chinese kanji), so the interface language reorders the two Han entries and nothing
/// else.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn fallback_faces(han: HanOrder) -> Vec<ScriptFace> {
    let han_scripts = ScriptSet::EMPTY.with(ScriptClass::Han);
    let ja = ScriptFace {
        key: "sys-cjk-han-ja",
        sans: cjk_han_japanese(),
        serif: Some(("sys-cjk-han-ja-serif", cjk_han_japanese_serif())),
        covers: han_scripts.with(ScriptClass::Japanese),
    };
    let zh = ScriptFace {
        key: "sys-cjk-han-zh",
        sans: cjk_han_simplified(),
        serif: Some(("sys-cjk-han-zh-serif", cjk_han_simplified_serif())),
        covers: han_scripts,
    };
    let (first, second) = match han {
        HanOrder::Japanese => (ja, zh),
        HanOrder::Chinese => (zh, ja),
    };
    // First, so an emoji code point reaches the colour face before the CJK face claims it: Noto Sans
    // CJK covers seventy `1Fxxx` code points of its own, in monochrome (#1231).
    let mut faces = vec![ScriptFace { key: "sys-emoji", sans: emoji(), serif: None, covers: ScriptSet::EMPTY.with(ScriptClass::Symbols) }];
    faces.extend([
        first,
        second,
        ScriptFace {
            key: "sys-cjk-hangul",
            sans: cjk_hangul(),
            serif: Some(("sys-cjk-hangul-serif", cjk_hangul_serif())),
            covers: ScriptSet::EMPTY.with(ScriptClass::Korean),
        },
        ScriptFace { key: "sys-thai", sans: thai(), serif: Some(("sys-thai-serif", thai_serif())), covers: ScriptSet::EMPTY.with(ScriptClass::Thai) },
        ScriptFace { key: "sys-lao", sans: lao(), serif: Some(("sys-lao-serif", lao_serif())), covers: ScriptSet::EMPTY.with(ScriptClass::Lao) },
        ScriptFace {
            key: "sys-khmer",
            sans: khmer(),
            serif: Some(("sys-khmer-serif", khmer_serif())),
            covers: ScriptSet::EMPTY.with(ScriptClass::Khmer),
        },
        ScriptFace {
            key: "sys-hebrew",
            sans: hebrew(),
            serif: Some(("sys-hebrew-serif", hebrew_serif())),
            covers: ScriptSet::EMPTY.with(ScriptClass::Hebrew),
        },
        ScriptFace {
            key: "sys-arabic",
            sans: arabic(),
            serif: Some(("sys-arabic-serif", arabic_serif())),
            covers: ScriptSet::EMPTY.with(ScriptClass::Arabic),
        },
        ScriptFace {
            key: "sys-devanagari",
            sans: devanagari(),
            serif: Some(("sys-devanagari-serif", devanagari_serif())),
            covers: ScriptSet::EMPTY.with(ScriptClass::Devanagari),
        },
        ScriptFace { key: "sys-myanmar", sans: myanmar(), serif: None, covers: ScriptSet::EMPTY.with(ScriptClass::Myanmar) },
        ScriptFace { key: "sys-tibetan", sans: tibetan(), serif: None, covers: ScriptSet::EMPTY.with(ScriptClass::Tibetan) },
        ScriptFace { key: "sys-yi", sans: yi(), serif: None, covers: ScriptSet::EMPTY.with(ScriptClass::Yi) },
        ScriptFace { key: "sys-symbols", sans: symbols(), serif: None, covers: ScriptSet::EMPTY.with(ScriptClass::Symbols) },
        ScriptFace { key: "sys-symbols-2", sans: symbols_more(), serif: None, covers: ScriptSet::EMPTY.with(ScriptClass::Symbols) },
    ]);
    faces
}

/// Linux directories that hold CJK fonts across distributions: Debian/Ubuntu (`opentype/noto`,
/// `truetype/wqy`), Fedora (`google-noto-cjk`), Arch (`noto-cjk`, `wenquanyi`), ChromeOS's Linux
/// container (`chromeos/notocjk`, #1762), plus local and flat layouts. These are fallbacks used
/// only after fontconfig has been asked.
#[cfg(not(target_arch = "wasm32"))]
const LINUX_CJK_FALLBACK_DIRS: [&str; 22] = [
    // Flatpak mounts host fonts here without a broad filesystem permission. Keep the same
    // distribution layouts as below; /usr/share/fonts inside the sandbox belongs to the runtime.
    "/run/host/fonts/opentype/noto",
    "/run/host/fonts/google-noto-cjk",
    "/run/host/fonts/noto-cjk",
    "/run/host/fonts/truetype/noto",
    "/run/host/fonts/noto",
    "/run/host/fonts/truetype",
    "/run/host/fonts/TTF",
    "/run/host/fonts",
    "/run/host/local-fonts",
    "/run/host/local-fonts/noto-cjk",
    "/run/host/user-fonts",
    "/usr/share/fonts/opentype/noto",
    "/usr/share/fonts/google-noto-cjk",
    "/usr/share/fonts/noto-cjk",
    "/usr/share/fonts/truetype/noto",
    "/usr/share/fonts/noto",
    "/usr/share/fonts/truetype",
    "/usr/share/fonts/TTF",
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/usr/local/share/fonts/noto-cjk",
    "/usr/share/fonts/chromeos/notocjk",
];

/// Japanese Han + Kana faces (Japanese kanji glyph shapes).
///
/// The region-merged `NotoSansCJK-Regular.ttc` holds one face per locale (JP 0, KR 1, SC 2, TC 3,
/// HK 4) with the same glyph coverage, so the index only picks glyph shapes. Whichever Han list
/// comes first loads it; [`load_fallback`] does not load the same file again for the other.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn cjk_han_japanese() -> Vec<(String, u32)> {
    let mac = "/System/Library/Fonts";
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let mut v: Vec<(String, u32)> = Vec::new();
    v.push((format!("{win}\\YuGothR.ttc"), 0)); // Yu Gothic
    v.push((format!("{win}\\msgothic.ttc"), 0)); // MS Gothic
    v.push((format!("{mac}/ヒラギノ角ゴシック W3.ttc"), 0)); // Hiragino Sans
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["sans-serif:lang=ja", "serif:lang=ja"]));
    for d in LINUX_CJK_FALLBACK_DIRS {
        v.push((format!("{d}/NotoSansCJKjp-Regular.otf"), 0));
        v.push((format!("{d}/NotoSansCJK-Regular.ttc"), 0)); // JP face
    }
    v
}

/// Simplified Chinese Han faces. Loaded apart from the Japanese ones so a glyph one family lacks
/// (simplified-only hanzi in a Japanese font) still falls through to a real glyph.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn cjk_han_simplified() -> Vec<(String, u32)> {
    let mac = "/System/Library/Fonts";
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let mut v: Vec<(String, u32)> = Vec::new();
    v.push((format!("{win}\\msyh.ttc"), 0)); // Microsoft YaHei
    v.push((format!("{win}\\simsun.ttc"), 0)); // SimSun
    v.push((format!("{win}\\simhei.ttf"), 0)); // SimHei
    v.push((format!("{mac}/Hiragino Sans GB.ttc"), 0)); // ships on every recent macOS
    v.push((format!("{mac}/PingFang.ttc"), 0)); // older macOS only; now a downloadable asset
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["sans-serif:lang=zh-cn", "serif:lang=zh-cn"]));
    for d in LINUX_CJK_FALLBACK_DIRS {
        v.push((format!("{d}/NotoSansCJKsc-Regular.otf"), 0));
        v.push((format!("{d}/NotoSansCJK-Regular.ttc"), 2)); // SC face
    }
    for d in LINUX_CJK_FALLBACK_DIRS {
        v.push((format!("{d}/wqy/wqy-microhei.ttc"), 0));
        v.push((format!("{d}/wenquanyi/wqy-microhei.ttc"), 0));
        v.push((format!("{d}/wqy-microhei.ttc"), 0));
    }
    v
}

/// Hangul faces (Korean). The Han faces above that come from the region-merged Noto Sans CJK file
/// already carry Hangul; [`load_fallback`] then skips this list rather than load the file twice.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn cjk_hangul() -> Vec<(String, u32)> {
    let mac = "/System/Library/Fonts";
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let mut v: Vec<(String, u32)> = Vec::new();
    v.push((format!("{win}\\malgun.ttf"), 0)); // Malgun Gothic
    v.push((format!("{mac}/AppleSDGothicNeo.ttc"), 0));
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["sans-serif:lang=ko", "serif:lang=ko"]));
    for d in LINUX_CJK_FALLBACK_DIRS {
        v.push((format!("{d}/NotoSansCJKkr-Regular.otf"), 0));
        v.push((format!("{d}/NotoSansCJK-Regular.ttc"), 1)); // KR face
    }
    v
}

/// Thai faces. Windows ships Leelawadee UI and Tahoma/Microsoft Sans Serif (both carry Thai);
/// macOS has Thonburi; Linux usually has Noto Sans Thai or Garuda.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn thai() -> Vec<(String, u32)> {
    let mac_sys = "/System/Library/Fonts";
    let mac_sup = "/System/Library/Fonts/Supplemental";
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let lin = [
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/truetype",
        "/usr/share/fonts",
        "/usr/share/fonts/chromeos/noto", // ChromeOS's Linux container (#1762)
    ];
    let mut v: Vec<(String, u32)> = Vec::new();
    v.push((format!("{win}\\LeelawUI.ttf"), 0)); // Leelawadee UI
    v.push((format!("{win}\\tahoma.ttf"), 0)); // Tahoma
    v.push((format!("{win}\\micross.ttf"), 0)); // Microsoft Sans Serif
    v.push((format!("{mac_sup}/Thonburi.ttc"), 0));
    v.push((format!("{mac_sys}/SukhumvitSet.ttc"), 0));
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["sans-serif:lang=th"]));
    for d in lin {
        v.push((format!("{d}/NotoSansThai-Regular.ttf"), 0));
        v.push((format!("{d}/tlwg/Garuda.ttf"), 0));
        v.push((format!("{d}/garuda/Garuda.ttf"), 0));
    }
    // Fedora ships the variable Noto Sans Thai; its default instance is Regular.
    v.push(("/usr/share/fonts/google-noto-vf/NotoSansThai[wght].ttf".into(), 0));
    v
}

/// Lao faces. Windows ships Leelawadee UI (and DokChampa on older versions); macOS has Lao Sangam
/// MN and Lao MN; Linux usually has Noto Sans Lao or Phetsarath OT.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn lao() -> Vec<(String, u32)> {
    let mac_sup = "/System/Library/Fonts/Supplemental";
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let lin = ["/usr/share/fonts/truetype/noto", "/usr/share/fonts/opentype/noto", "/usr/share/fonts/truetype", "/usr/share/fonts"];
    let mut v: Vec<(String, u32)> = Vec::new();
    v.push((format!("{win}\\LeelawUI.ttf"), 0)); // Leelawadee UI
    v.push((format!("{win}\\dokchamp.ttf"), 0)); // DokChampa
    v.push((format!("{mac_sup}/Lao Sangam MN.ttf"), 0));
    v.push((format!("{mac_sup}/Lao MN.ttc"), 0));
    for d in lin {
        v.push((format!("{d}/NotoSansLao-Regular.ttf"), 0));
        v.push((format!("{d}/lao/Phetsarath_OT.ttf"), 0));
        v.push((format!("{d}/Phetsarath_OT.ttf"), 0));
    }
    v
}

/// Hebrew faces. Windows has David and FrankRuehl (and Tahoma/Courier New carry Hebrew); macOS has
/// Arial Hebrew and David; Linux usually has Noto Sans Hebrew or DejaVu Sans.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn hebrew() -> Vec<(String, u32)> {
    let mac_sys = "/System/Library/Fonts";
    let mac_sup = "/System/Library/Fonts/Supplemental";
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let lin = [
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/truetype",
        "/usr/share/fonts",
        "/usr/share/fonts/chromeos/noto", // ChromeOS's Linux container (#1762)
    ];
    let mut v: Vec<(String, u32)> = Vec::new();
    v.push((format!("{win}\\david.ttf"), 0)); // David
    v.push((format!("{win}\\frank.ttf"), 0)); // FrankRuehl
    v.push((format!("{win}\\tahoma.ttf"), 0)); // Tahoma
    v.push((format!("{mac_sys}/ArialHB.ttc"), 0)); // Arial Hebrew
    v.push((format!("{mac_sys}/SFHebrew.ttf"), 0));
    v.push((format!("{mac_sup}/Arial Hebrew.ttf"), 0));
    v.push((format!("{mac_sup}/David.ttf"), 0));
    v.push((format!("{mac_sys}/LucidaGrande.ttc"), 0));
    v.push((format!("{mac_sys}/Lucida Grande.ttc"), 0));
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["sans-serif:lang=he"]));
    for d in lin {
        v.push((format!("{d}/NotoSansHebrew-Regular.ttf"), 0));
        v.push((format!("{d}/dejavu/DejaVuSans.ttf"), 0));
        v.push((format!("{d}/freefont/FreeSans.ttf"), 0));
    }
    v.push(("/usr/share/fonts/google-noto-vf/NotoSansHebrew[wght].ttf".into(), 0));
    v
}

/// Fallback candidates for a script's sans face: fontconfig first (distributions place fonts
/// differently), then the common file names in the standard Noto layouts.
#[cfg(not(target_arch = "wasm32"))]
fn script_face(lang: &str, files: &[&str]) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let pattern = format!("sans-serif:lang={lang}");
        v.extend(fc_match_candidates(&[pattern.as_str()]));
    }
    for d in ["/usr/share/fonts/google-noto-vf", "/usr/share/fonts/truetype/noto", "/usr/share/fonts/opentype/noto", "/usr/share/fonts"] {
        for file in files {
            v.push((format!("{d}/{file}"), 0));
        }
    }
    v
}

/// Same for a script's serif face. Asking fontconfig for `serif:lang=…` picks the serif design
/// (Noto Serif Thai, Naskh Arabic, …) where the plain name would return the sans one.
#[cfg(not(target_arch = "wasm32"))]
fn script_serif(lang: &str, files: &[&str]) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let pattern = format!("serif:lang={lang}");
        v.extend(fc_match_candidates(&[pattern.as_str()]));
    }
    for d in ["/usr/share/fonts/google-noto-vf", "/usr/share/fonts/truetype/noto", "/usr/share/fonts/opentype/noto", "/usr/share/fonts"] {
        for file in files {
            v.push((format!("{d}/{file}"), 0));
        }
    }
    v
}

#[cfg(not(target_arch = "wasm32"))]
fn lao_serif() -> Vec<(String, u32)> {
    script_serif("lo", &["NotoSerifLao-Regular.ttf", "NotoSerifLao[wght].ttf"])
}

/// Khmer faces.
#[cfg(not(target_arch = "wasm32"))]
fn khmer() -> Vec<(String, u32)> {
    script_face("km", &["NotoSansKhmer-Regular.ttf", "NotoSansKhmer[wght].ttf"])
}

#[cfg(not(target_arch = "wasm32"))]
fn khmer_serif() -> Vec<(String, u32)> {
    script_serif("km", &["NotoSerifKhmer-Regular.ttf", "NotoSerifKhmer[wght].ttf"])
}

/// Arabic faces. The serif companion is Naskh (the traditional book design), not a Latin serif.
#[cfg(not(target_arch = "wasm32"))]
fn arabic() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    // The face fontconfig prefers for Arabic, such as a Vazirmatn the user set as their Arabic
    // font (#229), leads the script faces when it actually has Arabic letters.
    #[cfg(all(unix, not(target_os = "macos")))]
    v.extend(fontconfig_arabic());
    v.extend(script_face("ar", &["NotoSansArabic-Regular.ttf", "NotoSansArabic[wdth,wght].ttf"]));
    v
}

#[cfg(not(target_arch = "wasm32"))]
fn arabic_serif() -> Vec<(String, u32)> {
    script_serif("ar", &["NotoNaskhArabic-Regular.ttf", "NotoNaskhArabic[wght].ttf"])
}

/// Devanagari faces.
#[cfg(not(target_arch = "wasm32"))]
fn devanagari() -> Vec<(String, u32)> {
    script_face("hi", &["NotoSansDevanagari-Regular.ttf", "NotoSansDevanagari[wdth,wght].ttf"])
}

#[cfg(not(target_arch = "wasm32"))]
fn devanagari_serif() -> Vec<(String, u32)> {
    script_serif("hi", &["NotoSerifDevanagari-Regular.ttf", "NotoSerifDevanagari[wdth,wght].ttf"])
}

/// Myanmar faces (Padauk is the common one; it has no serif companion).
#[cfg(not(target_arch = "wasm32"))]
fn myanmar() -> Vec<(String, u32)> {
    script_face("my", &["Padauk-Regular.ttf"])
}

/// Tibetan faces (Jomolhari is the common one; no serif companion).
#[cfg(not(target_arch = "wasm32"))]
fn tibetan() -> Vec<(String, u32)> {
    script_face("bo", &["Jomolhari-Regular.ttf"])
}

/// Yi (Nuosu) faces; Noto Sans Yi has no serif companion.
#[cfg(not(target_arch = "wasm32"))]
fn yi() -> Vec<(String, u32)> {
    script_face("ii", &["NotoSansYi-Regular.ttf"])
}

/// Thai serif faces (Noto Serif Thai where installed).
#[cfg(not(target_arch = "wasm32"))]
fn thai_serif() -> Vec<(String, u32)> {
    script_serif("th", &["NotoSerifThai-Regular.ttf", "NotoSerifThai[wght].ttf"])
}

/// Hebrew serif faces (Noto Serif Hebrew where installed).
#[cfg(not(target_arch = "wasm32"))]
fn hebrew_serif() -> Vec<(String, u32)> {
    script_serif("he", &["NotoSerifHebrew-Regular.ttf", "NotoSerifHebrew[wght].ttf"])
}

/// Japanese Han serif faces (Noto Serif CJK JP). The region-merged `NotoSerifCJK-Regular.ttc`
/// carries one face per locale (JP 0, KR 1, SC 2, TC 3, HK 4).
#[cfg(not(target_arch = "wasm32"))]
fn cjk_han_japanese_serif() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["serif:lang=ja"]));
    for d in LINUX_CJK_FALLBACK_DIRS {
        v.push((format!("{d}/NotoSerifCJKjp-Regular.otf"), 0));
        v.push((format!("{d}/NotoSerifCJK-Regular.ttc"), 0));
    }
    v
}

#[cfg(not(target_arch = "wasm32"))]
fn cjk_han_simplified_serif() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["serif:lang=zh-cn"]));
    for d in LINUX_CJK_FALLBACK_DIRS {
        v.push((format!("{d}/NotoSerifCJKsc-Regular.otf"), 0));
        v.push((format!("{d}/NotoSerifCJK-Regular.ttc"), 2));
    }
    v
}

#[cfg(not(target_arch = "wasm32"))]
fn cjk_hangul_serif() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["serif:lang=ko"]));
    for d in LINUX_CJK_FALLBACK_DIRS {
        v.push((format!("{d}/NotoSerifCJKkr-Regular.otf"), 0));
        v.push((format!("{d}/NotoSerifCJK-Regular.ttc"), 1));
    }
    v
}


/// Symbol faces behind the UI font. Interface fonts such as Noto Sans and Cantarell (and egui's
/// built-in Ubuntu) lack arrows, check marks, geometric shapes and keyboard symbols (▾ ✓ ✕ ▲ ↳ ⇧),
/// which then render as empty boxes (#197, #54). DejaVu Sans covers most of them; the paths cover
/// Debian/Ubuntu, Fedora and Arch layouts. [`load_fallback`] skips the file if Hebrew already
/// loaded it.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn symbols() -> Vec<(String, u32)> {
    let win = std::env::var("WINDIR").map(|w| format!("{w}\\Fonts")).unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    let mut v: Vec<(String, u32)> = vec![(format!("{win}\\seguisym.ttf"), 0), ("/System/Library/Fonts/Apple Symbols.ttf".into(), 0)];
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["DejaVu Sans", "Symbola", "Noto Sans Symbols 2", "Noto Sans Symbols"]));
    for d in [
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts/dejavu-sans-fonts",
        "/usr/share/fonts/TTF",
        "/usr/share/fonts/dejavu",
        "/usr/share/fonts/truetype/ttf-dejavu",
        "/usr/local/share/fonts/dejavu",
        "/usr/local/share/fonts",
    ] {
        v.push((format!("{d}/DejaVuSans.ttf"), 0));
    }
    v
}

/// A second symbol face for what the first lacks (⌘ and other technical symbols): Noto Sans
/// Symbols 2 where it is installed, or Segoe UI Symbol behind Segoe UI.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn symbols_more() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "linux")]
    v.extend(fc_match_candidates(&["Noto Sans Symbols 2", "Symbola", "Noto Sans Symbols"]));
    for d in [
        "/usr/share/fonts/noto",
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/google-noto-sans-symbols-2-fonts",
        "/usr/share/fonts/google-noto",
        "/usr/local/share/fonts/noto",
        "/usr/share/fonts/chromeos/noto",
    ] {
        v.push((format!("{d}/NotoSansSymbols2-Regular.ttf"), 0));
    }
    v
}

/// Whether any symbol fallback face is installed (tests skip their glyph checks without one).
pub fn symbol_face_installed() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        symbols().iter().chain(symbols_more().iter()).any(|(p, _)| std::path::Path::new(p).exists())
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}
