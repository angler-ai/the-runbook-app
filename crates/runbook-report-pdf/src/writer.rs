//! Serialize laid-out pages as PDF 1.7: a classic cross-reference table,
//! Type0/CIDFontType2 fonts with Identity-H encoding (text is glyph ids in
//! hex strings), a ToUnicode map per font so text can be copied and
//! searched, and uncompressed page content streams.
use crate::fonts::FontId;
use crate::layout::{Color, Document, Op, PAGE_HEIGHT, PAGE_WIDTH};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub fn write(document: &Document) -> Vec<u8> {
    let mut pdf = Pdf::default();
    let fonts: Vec<FontId> = document.used.keys().copied().collect();
    // Object numbers, fixed up front.
    let catalog = 1;
    let pages = 2;
    let info = 3;
    let resources = 4;
    let mut next = 5;
    let mut font_objects = BTreeMap::new();
    for font in &fonts {
        font_objects.insert(*font, next);
        next += 5;
    }
    let page_objects: Vec<usize> = (0..document.pages.len()).map(|i| next + 2 * i).collect();

    pdf.object(catalog, &format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    let kids: Vec<String> = page_objects.iter().map(|id| format!("{id} 0 R")).collect();
    pdf.object(
        pages,
        &format!(
            "<< /Type /Pages /Kids [{}] /Count {} /MediaBox [0 0 {} {}] >>",
            kids.join(" "),
            page_objects.len(),
            number(PAGE_WIDTH),
            number(PAGE_HEIGHT)
        ),
    );
    pdf.object(
        info,
        &format!(
            "<< /Title {} /Producer (The Runbook) >>",
            utf16_hex(&document.title)
        ),
    );
    let font_refs: Vec<String> = font_objects
        .iter()
        .map(|(font, id)| format!("/{} {id} 0 R", resource_name(*font)))
        .collect();
    pdf.object(
        resources,
        &format!("<< /Font << {} >> >>", font_refs.join(" ")),
    );
    for (font, first) in &font_objects {
        let glyphs = &document.used[font];
        write_font(&mut pdf, document, *font, *first, glyphs);
    }
    for (page, id) in document.pages.iter().zip(&page_objects) {
        pdf.object(
            *id,
            &format!(
                "<< /Type /Page /Parent {pages} 0 R /Resources {resources} 0 R /Contents {} 0 R >>",
                id + 1
            ),
        );
        pdf.stream(*id + 1, "", content(page).as_bytes());
    }
    pdf.finish(catalog, info)
}

fn write_font(
    pdf: &mut Pdf,
    document: &Document,
    font: FontId,
    first: usize,
    glyphs: &BTreeMap<u16, (char, i32)>,
) {
    let (type0, cid, descriptor, file, to_unicode) =
        (first, first + 1, first + 2, first + 3, first + 4);
    let name = font.base_name();
    pdf.object(
        type0,
        &format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H \
             /DescendantFonts [{cid} 0 R] /ToUnicode {to_unicode} 0 R >>"
        ),
    );
    let mut widths = String::new();
    for (glyph, (_, width)) in glyphs {
        let _ = write!(widths, "{glyph} [{width}] ");
    }
    pdf.object(
        cid,
        &format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /FontDescriptor {descriptor} 0 R /DW 1000 /W [{}] /CIDToGIDMap /Identity >>",
            widths.trim_end()
        ),
    );
    let metrics = document.fonts.metrics(font);
    let flags = if font.monospaced() { 32 | 1 } else { 32 };
    let [x0, y0, x1, y1] = metrics.bbox;
    pdf.object(
        descriptor,
        &format!(
            "<< /Type /FontDescriptor /FontName /{name} /Flags {flags} \
             /FontBBox [{x0} {y0} {x1} {y1}] /ItalicAngle 0 /Ascent {} /Descent {} \
             /CapHeight {} /StemV 80 /FontFile2 {file} 0 R >>",
            metrics.ascent, metrics.descent, metrics.cap_height
        ),
    );
    pdf.stream(
        file,
        &format!("/Length1 {} /Filter /FlateDecode", font.data().len()),
        font.compressed(),
    );
    pdf.stream(to_unicode, "", cmap(glyphs).as_bytes());
}

/// The ToUnicode CMap: one `bfchar` entry per glyph drawn.
fn cmap(glyphs: &BTreeMap<u16, (char, i32)>) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &(char, i32))> = glyphs.iter().collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (glyph, (c, _)) in chunk {
            let mut units = [0u16; 2];
            let hex: String = c
                .encode_utf16(&mut units)
                .iter()
                .map(|u| format!("{u:04X}"))
                .collect();
            let _ = writeln!(out, "<{glyph:04X}> <{hex}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

/// A page's content stream.
fn content(ops: &[Op]) -> String {
    let mut out = String::new();
    for op in ops {
        match op {
            Op::Text {
                x,
                y,
                size,
                italic,
                color,
                runs,
            } => {
                let shear = if *italic { "0.2" } else { "0" };
                let _ = write!(
                    out,
                    "BT {} rg 1 0 {shear} 1 {} {} Tm",
                    rgb(*color),
                    number(*x),
                    number(*y)
                );
                for run in runs {
                    let hex: String = run.glyphs.iter().map(|g| format!("{g:04X}")).collect();
                    let _ = write!(
                        out,
                        " /{} {} Tf <{hex}> Tj",
                        resource_name(run.font),
                        number(*size)
                    );
                }
                out.push_str(" ET\n");
            }
            Op::Fill { x, y, w, h, color } => {
                let _ = writeln!(
                    out,
                    "{} rg {} {} {} {} re f",
                    rgb(*color),
                    number(*x),
                    number(*y),
                    number(*w),
                    number(*h)
                );
            }
            Op::Stroke { x, y, w, h, color } => {
                let _ = writeln!(
                    out,
                    "{} RG 0.5 w {} {} {} {} re S",
                    rgb(*color),
                    number(*x),
                    number(*y),
                    number(*w),
                    number(*h)
                );
            }
            Op::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => {
                let _ = writeln!(
                    out,
                    "{} RG {} w {} {} m {} {} l S",
                    rgb(*color),
                    number(*width),
                    number(*x1),
                    number(*y1),
                    number(*x2),
                    number(*y2)
                );
            }
        }
    }
    out
}

fn resource_name(font: FontId) -> String {
    format!("F{}", font as usize + 1)
}

fn rgb(Color(r, g, b): Color) -> String {
    let channel = |c: u8| number(f64::from(c) / 255.0);
    format!("{} {} {}", channel(r), channel(g), channel(b))
}

/// A fixed-precision number: at most two decimals, no exponent, no `-0`.
fn number(value: f64) -> String {
    let hundredths = (value * 100.0).round() as i64;
    let sign = if hundredths < 0 { "-" } else { "" };
    let abs = hundredths.unsigned_abs();
    let (whole, fraction) = (abs / 100, abs % 100);
    match fraction {
        0 => format!("{sign}{whole}"),
        f if f % 10 == 0 => format!("{sign}{whole}.{}", f / 10),
        f => format!("{sign}{whole}.{f:02}"),
    }
}

/// A text string as UTF-16BE hex with a byte-order mark: no character of
/// the text can end the string or the dictionary.
fn utf16_hex(text: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in text.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
    out
}

#[derive(Default)]
struct Pdf {
    bytes: Vec<u8>,
    offsets: BTreeMap<usize, usize>,
}

impl Pdf {
    fn start(&mut self, id: usize) {
        if self.bytes.is_empty() {
            // A binary comment marks the file as binary for transports.
            self.bytes
                .extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        }
        self.offsets.insert(id, self.bytes.len());
        self.bytes
            .extend_from_slice(format!("{id} 0 obj\n").as_bytes());
    }

    fn object(&mut self, id: usize, body: &str) {
        self.start(id);
        self.bytes.extend_from_slice(body.as_bytes());
        self.bytes.extend_from_slice(b"\nendobj\n");
    }

    fn stream(&mut self, id: usize, extra: &str, data: &[u8]) {
        self.start(id);
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!(" {extra}")
        };
        self.bytes
            .extend_from_slice(format!("<< /Length {}{extra} >>\nstream\n", data.len()).as_bytes());
        self.bytes.extend_from_slice(data);
        self.bytes.extend_from_slice(b"\nendstream\nendobj\n");
    }

    fn finish(mut self, root: usize, info: usize) -> Vec<u8> {
        let size = self.offsets.keys().max().map_or(1, |max| max + 1);
        let xref = self.bytes.len();
        let mut table = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for id in 1..size {
            match self.offsets.get(&id) {
                Some(offset) => {
                    let _ = writeln!(table, "{offset:010} 00000 n ");
                }
                None => table.push_str("0000000000 65535 f \n"),
            }
        }
        let _ = write!(
            table,
            "trailer\n<< /Size {size} /Root {root} 0 R /Info {info} 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        );
        self.bytes.extend_from_slice(table.as_bytes());
        self.bytes
    }
}
