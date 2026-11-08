//! Windows, macOS and Linux system font discovery and TTC extraction for PDF text editing.
//!
//! When editing existing PDF text (especially CJK/Korean text), source subset fonts
//! often lack glyphs for new characters. This module matches system fonts
//! (e.g. Malgun Gothic, Batang, Gulim) and extracts standalone TrueType programs
//! from TrueType Collections (`.ttc`), so they can be embedded into the PDF as Type0 fonts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use skrifa::{FontRef, MetadataProvider};

/// Extracts a standalone TrueType font from a TrueType Collection (`.ttc`).
/// If `bytes` is not a collection (does not start with `ttcf`), returns `Some(bytes.to_vec())` when `face_index == 0`.
pub fn extract_ttc_face(bytes: &[u8], face_index: u32) -> Option<Vec<u8>> {
    if !bytes.starts_with(b"ttcf") {
        return if face_index == 0 { Some(bytes.to_vec()) } else { None };
    }
    if bytes.len() < 12 {
        return None;
    }
    let num_fonts = u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?);
    if face_index >= num_fonts {
        return None;
    }
    let offset_pos = 12usize.checked_add((face_index as usize).checked_mul(4)?)?;
    let table_dir_offset = u32::from_be_bytes(bytes.get(offset_pos..offset_pos.checked_add(4)?)?.try_into().ok()?) as usize;
    if table_dir_offset.checked_add(12)? > bytes.len() {
        return None;
    }
    let num_tables = u16::from_be_bytes(bytes.get(table_dir_offset.checked_add(4)?..table_dir_offset.checked_add(6)?)?.try_into().ok()?) as usize;
    let header = bytes.get(table_dir_offset..table_dir_offset.checked_add(12)?)?;
    let dir_end = table_dir_offset.checked_add(12)?.checked_add(num_tables.checked_mul(16)?)?;
    let table_dir_raw = bytes.get(table_dir_offset.checked_add(12)?..dir_end)?;

    struct TableEntry<'a> {
        tag: [u8; 4],
        checksum: [u8; 4],
        data: &'a [u8],
    }

    let mut tables = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let entry = table_dir_raw.get(i * 16..(i + 1) * 16)?;
        let tag: [u8; 4] = entry.get(0..4)?.try_into().ok()?;
        let checksum: [u8; 4] = entry.get(4..8)?.try_into().ok()?;
        let toff = u32::from_be_bytes(entry.get(8..12)?.try_into().ok()?) as usize;
        let tlen = u32::from_be_bytes(entry.get(12..16)?.try_into().ok()?) as usize;
        let data = bytes.get(toff..toff.checked_add(tlen)?)?;
        tables.push(TableEntry { tag, checksum, data });
    }

    let mut dir = Vec::with_capacity(num_tables * 16);
    let mut cur_offset = 12usize.checked_add(num_tables.checked_mul(16)?)?;
    for entry in &tables {
        while cur_offset % 4 != 0 {
            cur_offset = cur_offset.checked_add(1)?;
        }
        dir.extend_from_slice(&entry.tag);
        dir.extend_from_slice(&entry.checksum);
        dir.extend_from_slice(&(u32::try_from(cur_offset).ok()?.to_be_bytes()));
        dir.extend_from_slice(&(u32::try_from(entry.data.len()).ok()?.to_be_bytes()));
        cur_offset = cur_offset.checked_add(entry.data.len())?;
    }

    let mut out = Vec::with_capacity(cur_offset);
    out.extend_from_slice(header);
    out.extend_from_slice(&dir);
    for entry in &tables {
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(entry.data);
    }
    Some(out)
}

/// Standard system fonts directory.
pub fn system_font_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(windir) = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")) {
            dirs.push(PathBuf::from(windir).join("Fonts"));
        } else {
            dirs.push(PathBuf::from(r"C:\Windows\Fonts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Fonts"));
        }
    } else {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = std::env::var_os("HOME") {
            let home_path = PathBuf::from(home);
            dirs.push(home_path.join(".local/share/fonts"));
            dirs.push(home_path.join(".fonts"));
        }
    }
    dirs
}

/// Normalized key for matching font family names (case-insensitive, alphanumeric only).
pub fn font_match_key(name: &str) -> String {
    let clean = name.rsplit('+').next().unwrap_or(name);
    clean.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Read file bytes with caching.
fn cached_file_bytes(path: &Path) -> Option<Arc<Vec<u8>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<u8>>>>> = OnceLock::new();
    let cache_mutex = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache_mutex.lock() {
        if let Some(bytes) = guard.get(path) {
            return Some(Arc::clone(bytes));
        }
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > (64 << 20) {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let arc = Arc::new(data);
    if let Ok(mut guard) = cache_mutex.lock() {
        guard.insert(path.to_path_buf(), Arc::clone(&arc));
    }
    Some(arc)
}

/// Candidate specification: filename and face index within the file.
struct FontCandidate {
    filename: &'static str,
    face_index: u32,
    family_name: &'static str,
}

/// Return candidates based on source font name and writing style.
fn find_candidates(base_font: &str) -> Vec<FontCandidate> {
    let key = font_match_key(base_font);
    let mut candidates = Vec::new();

    let is_serif = key.contains("batang")
        || key.contains("myeongjo")
        || key.contains("바탕")
        || key.contains("명조")
        || key.contains("times")
        || key.contains("serif")
        || key.contains("roman")
        || key.contains("mincho");

    let is_bold = key.contains("bold") || key.contains("bd") || key.contains("black") || key.contains("heavy");

    // Specific font matches
    if key.contains("malgun") || key.contains("맑은") {
        if is_bold {
            candidates.push(FontCandidate { filename: "malgunbd.ttf", face_index: 0, family_name: "Malgun Gothic" });
        }
        candidates.push(FontCandidate { filename: "malgun.ttf", face_index: 0, family_name: "Malgun Gothic" });
    } else if key.contains("batang") || key.contains("바탕") {
        candidates.push(FontCandidate { filename: "batang.ttc", face_index: 0, family_name: "Batang" });
        candidates.push(FontCandidate { filename: "batang.ttc", face_index: 1, family_name: "BatangChe" });
    } else if key.contains("gungsuh") || key.contains("궁서") {
        candidates.push(FontCandidate { filename: "batang.ttc", face_index: 2, family_name: "Gungsuh" });
    } else if key.contains("gulim") || key.contains("굴림") {
        candidates.push(FontCandidate { filename: "gulim.ttc", face_index: 0, family_name: "Gulim" });
        candidates.push(FontCandidate { filename: "gulim.ttc", face_index: 1, family_name: "GulimChe" });
    } else if key.contains("dotum") || key.contains("돋움") {
        candidates.push(FontCandidate { filename: "gulim.ttc", face_index: 2, family_name: "Dotum" });
        candidates.push(FontCandidate { filename: "gulim.ttc", face_index: 3, family_name: "DotumChe" });
    } else if key.contains("nanumgothic") || key.contains("나눔고딕") {
        candidates.push(FontCandidate { filename: "NanumGothic.ttf", face_index: 0, family_name: "NanumGothic" });
    } else if key.contains("nanummyeongjo") || key.contains("나눔명조") {
        candidates.push(FontCandidate { filename: "NanumMyeongjo.ttf", face_index: 0, family_name: "NanumMyeongjo" });
    } else if key.contains("h2hdr") || key.contains("휴먼헤드라인") {
        candidates.push(FontCandidate { filename: "H2HDRM.TTF", face_index: 0, family_name: "HYHeadLine-Medium" });
    } else if key.contains("h2gtr") || key.contains("휴먼고딕") {
        candidates.push(FontCandidate { filename: "H2GTRM.TTF", face_index: 0, family_name: "HYGoThic-Medium" });
    } else if key.contains("hcrbatang") || key.contains("함초롬바탕") {
        candidates.push(FontCandidate { filename: "HCRBATANG.TTF", face_index: 0, family_name: "HCR Batang" });
    } else if key.contains("hcrdotum") || key.contains("함초롬돋움") {
        candidates.push(FontCandidate { filename: "HCRDOTUM.TTF", face_index: 0, family_name: "HCR Dotum" });
    }

    // Default fallbacks according to style
    if is_serif {
        candidates.push(FontCandidate { filename: "batang.ttc", face_index: 0, family_name: "Batang" });
        candidates.push(FontCandidate { filename: "malgun.ttf", face_index: 0, family_name: "Malgun Gothic" });
    } else {
        if is_bold {
            candidates.push(FontCandidate { filename: "malgunbd.ttf", face_index: 0, family_name: "Malgun Gothic" });
        }
        candidates.push(FontCandidate { filename: "malgun.ttf", face_index: 0, family_name: "Malgun Gothic" });
        candidates.push(FontCandidate { filename: "gulim.ttc", face_index: 0, family_name: "Gulim" });
        candidates.push(FontCandidate { filename: "batang.ttc", face_index: 0, family_name: "Batang" });
    }

    // macOS fallbacks
    candidates.push(FontCandidate { filename: "AppleSDGothicNeo.ttc", face_index: 0, family_name: "Apple SD Gothic Neo" });
    candidates.push(FontCandidate { filename: "AppleGothic.ttf", face_index: 0, family_name: "AppleGothic" });

    candidates
}

/// Try to load a standalone font matching `base_font` that can display every character in `text`.
///
/// Returns `Some((standalone_ttf_bytes, family_name))` on success.
/// Returns true if character is in Korean Unicode blocks.
pub fn is_korean(c: char) -> bool {
    matches!(c, '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' | '\u{A960}'..='\u{A97F}' | '\u{D7B0}'..='\u{D7FF}')
}

/// Returns true if string contains any Korean characters.
pub fn has_korean(s: &str) -> bool {
    s.chars().any(is_korean)
}

fn is_korean_font_name(base: &str) -> bool {
    let lower = base.to_lowercase();
    lower.contains("malgun") || lower.contains("batang") || lower.contains("gulim") 
        || lower.contains("dotum") || lower.contains("nanum") || lower.contains("korean")
}

pub fn system_matching_font(base_font: &str, text: &str) -> Option<(Vec<u8>, String)> {
    if !has_korean(text) && !is_korean_font_name(base_font) {
        return None;
    }

    let dirs = system_font_directories();
    let candidates = find_candidates(base_font);

    for cand in candidates {
        for dir in &dirs {
            let path = dir.join(cand.filename);
            let Some(file_bytes) = cached_file_bytes(&path) else { continue };
            let Some(standalone) = extract_ttc_face(&file_bytes, cand.face_index) else { continue };
            let Ok(font) = FontRef::new(&standalone) else { continue };
            let charmap = font.charmap();

            // Check if all non-whitespace characters in text are covered
            let all_covered = text.chars().all(|c| {
                if c.is_whitespace() {
                    return true;
                }
                charmap.map(c).is_some_and(|g| g.to_u32() != 0)
            });

            if all_covered {
                return Some((standalone, cand.family_name.to_string()));
            }
        }
    }
    None
}

/// Return Korean UI font bytes if available on the system (for egui fallback).
pub fn system_korean_ui_font() -> Option<Vec<u8>> {
    let dirs = system_font_directories();
    let candidates = ["malgun.ttf", "batang.ttc", "gulim.ttc", "NanumGothic.ttf"];
    for name in candidates {
        for dir in &dirs {
            let path = dir.join(name);
            let Some(file_bytes) = cached_file_bytes(&path) else { continue };
            if let Some(standalone) = extract_ttc_face(&file_bytes, 0) {
                if FontRef::new(&standalone).is_ok() {
                    return Some(standalone);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_matching_font_korean() {
        if cfg!(windows) {
            let res = system_matching_font("MalgunGothic", "가나다 맑은고딕 테스트");
            assert!(res.is_some(), "Should find a matching font for Korean text on Windows");
            let (bytes, family) = res.unwrap();
            assert!(!bytes.is_empty());
            assert!(!family.is_empty());

            // Test serif / batang
            let res_serif = system_matching_font("Batang", "바탕체 한글 테스트");
            assert!(res_serif.is_some(), "Should find Batang for serif Korean text");
        }
    }
}

