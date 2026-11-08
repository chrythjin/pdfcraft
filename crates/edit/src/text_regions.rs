//! Geometry-based regions and mixed-style rewriting of existing text.

use std::sync::atomic::{AtomicU8, Ordering};

/// Text grouping granularity mode (FlexiPDF style).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Granularity {
    /// Full paragraph grouping (default)
    Paragraph = 0,
    /// Line-by-line grouping
    Line = 1,
    /// Individual word grouping
    Word = 2,
}

thread_local! {
    static THREAD_GRANULARITY: AtomicU8 = const { AtomicU8::new(0) };
}
static GLOBAL_GRANULARITY: AtomicU8 = AtomicU8::new(0);

/// Set text editing granularity mode at runtime.
pub fn set_granularity(g: Granularity) {
    GLOBAL_GRANULARITY.store(g as u8, Ordering::Relaxed);
    THREAD_GRANULARITY.with(|atom| atom.store(g as u8, Ordering::Relaxed));
}

/// Get current text editing granularity mode.
pub fn current_granularity() -> Granularity {
    if let Some(v) = std::env::var_os("PDFCRAFT_GRANULARITY") {
        if v == "word" || v == "Word" {
            return Granularity::Word;
        } else if v == "line" || v == "Line" {
            return Granularity::Line;
        } else if v == "para" || v == "Para" || v == "paragraph" || v == "Paragraph" {
            return Granularity::Paragraph;
        }
    }
    let thread_val = THREAD_GRANULARITY.with(|atom| atom.load(Ordering::Relaxed));
    let val = if thread_val != 0 { thread_val } else { GLOBAL_GRANULARITY.load(Ordering::Relaxed) };
    match val {
        1 => Granularity::Line,
        2 => Granularity::Word,
        _ => Granularity::Paragraph,
    }
}

use super::{
    BlockStyle, CraftFont, Dict, Document, EditError, HashMap, LineEdit, MAX_TYPE3_GLYPHS, Matrix, Metrics, Object, Op, PdfString, Pieces,
    RangeStyle, TextBlock, TextLine, Type3Glyph, coincident_ops, content_streams, fallback_face, japanese_glyph_from, needs_type3, no_fallback_font,
    no_glyph_for, page_dict, paragraph_separator, reveal_ops, rewritten_contents, same_base_font, same_font, source_encodes, source_family,
    source_program, source_show, source_space, source_width, splice, strip_subset_tag, type3_faces, type3_glyph, union, write_type3_glyphs,
};

#[path = "text_regions/grouping.rs"]
mod grouping;
#[path = "text_regions/styled.rs"]
mod styled;


pub(super) use grouping::{alignment_frame, group_blocks, group_reading_blocks, is_centred};
pub(super) use styled::rewrite_styled_block;
