//! D52: the execution report as PDF. The input is the same Markdown the
//! `?format=markdown` export serves (so the custom report already follows
//! D34 and untrusted text is already escaped there); it is parsed with
//! `pulldown-cmark` and laid out by a small PDF writer.
//!
//! - Handles headings, paragraphs, emphasis, lists, block quotes, tables,
//!   code blocks, rules and links (as their text plus the target). Images
//!   come later; for now their alt text is shown.
//! - Text is drawn with embedded fonts as glyph ids in hex strings, so no
//!   untrusted character can reach PDF syntax; the document title in the
//!   information dictionary is a UTF-16 hex string for the same reason.
//! - Deterministic: no clock (no creation or modification date is written),
//!   no random document id, ordered maps only, fixed number formatting.
//!   The same Markdown always gives the same bytes.
//! - Fonts (SIL Open Font License 1.1, see `fonts/LICENSE-*.txt`): Inter
//!   Regular and SemiBold (Latin subset), Inter Regular (Latin Extended
//!   subset, used only for characters the Latin subset lacks) and
//!   JetBrains Mono Regular (Latin subset) for code. They are the
//!   `@fontsource/inter` and `@fontsource/jetbrains-mono` 5.3.0 WOFF files
//!   the web app ships, unpacked losslessly to TrueType. Neither family
//!   declares a Reserved Font Name. Only the fonts a document uses are
//!   embedded; a character no font covers is drawn as `?`.
mod fonts;
pub mod inspect;
mod layout;
mod writer;

/// Render the Markdown report as a PDF file.
pub fn render_pdf(markdown: &str) -> Vec<u8> {
    let document = layout::lay_out(markdown);
    writer::write(&document)
}
