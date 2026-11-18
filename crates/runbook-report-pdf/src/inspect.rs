//! A small reader for this crate's own output, for tests here and in the
//! crates that serve the PDF. It is not a general PDF parser: it reads the
//! uncompressed page content streams and the ToUnicode maps this writer
//! emits, and checks the cross-reference table. Nothing calls it at runtime.
use std::collections::BTreeMap;

/// One indirect object: its dictionary text and its (decoded) stream.
struct Object {
    dict: String,
    stream: Option<Vec<u8>>,
}

/// Check the file's frame: the `%PDF-` header, an `%%EOF` trailer, a
/// `startxref` that points at an `xref` table, and an entry for every object
/// that points exactly at `N 0 obj`.
pub fn check_structure(pdf: &[u8]) -> Result<(), String> {
    if !pdf.starts_with(b"%PDF-1.") {
        return Err("missing %PDF- header".into());
    }
    if !pdf.ends_with(b"%%EOF\n") {
        return Err("missing %%EOF".into());
    }
    let tail = find_last(pdf, b"startxref\n").ok_or("missing startxref")?;
    let offset: usize = line_at(pdf, tail + b"startxref\n".len())
        .trim()
        .parse()
        .map_err(|_| "bad startxref")?;
    if !pdf[offset..].starts_with(b"xref\n") {
        return Err(format!("startxref {offset} does not point at xref"));
    }
    let mut at = offset + b"xref\n".len();
    let header = line_at(pdf, at);
    at += header.len() + 1;
    let mut parts = header.split_whitespace();
    let first: usize = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or("bad xref")?;
    let count: usize = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or("bad xref")?;
    if first != 0 {
        return Err("xref must start at object 0".into());
    }
    for number in 0..count {
        let entry = pdf.get(at..at + 20).ok_or("short xref")?;
        at += 20;
        if number == 0 {
            continue;
        }
        let entry = std::str::from_utf8(entry).map_err(|_| "bad xref entry")?;
        let target: usize = entry[..10].parse().map_err(|_| "bad xref offset")?;
        let expected = format!("{number} 0 obj\n");
        if !pdf[target..].starts_with(expected.as_bytes()) {
            return Err(format!(
                "xref entry {number} points at {target}, not its object"
            ));
        }
    }
    let trailer = std::str::from_utf8(&pdf[at..]).map_err(|_| "bad trailer")?;
    if !trailer.starts_with("trailer\n") || !trailer.contains(&format!("/Size {count} ")) {
        return Err(format!("trailer does not declare /Size {count}"));
    }
    Ok(())
}

/// The number of pages, from the page tree's `/Count`.
pub fn page_count(pdf: &[u8]) -> usize {
    let objects = objects(pdf);
    let root = catalog_pages(&objects);
    root.and_then(|pages| objects.get(&pages))
        .and_then(|pages| number_after(&pages.dict, "/Count "))
        .unwrap_or(0)
}

/// The document title from the information dictionary, decoding either a
/// UTF-16BE hex string or a literal string with its escapes.
pub fn info_title(pdf: &[u8]) -> Option<String> {
    let objects = objects(pdf);
    let trailer = std::str::from_utf8(&pdf[find_last(pdf, b"trailer\n")?..]).ok()?;
    let info = reference_after(trailer, "/Info ")?;
    let dict = &objects.get(&info)?.dict;
    let at = dict.find("/Title")? + "/Title".len();
    let rest = dict[at..].trim_start();
    if let Some(hex) = rest.strip_prefix('<') {
        let bytes = hex_bytes(&hex[..hex.find('>')?]);
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]))
            .collect();
        let text = String::from_utf16(&units).ok()?;
        return Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned());
    }
    literal_string(rest.strip_prefix('(')?)
}

/// The text of every page in order: one line per text line drawn, glyphs
/// mapped back to characters through each font's ToUnicode map. Pages are
/// separated by a form feed.
pub fn extract_text(pdf: &[u8]) -> String {
    let objects = objects(pdf);
    let Some(pages) = catalog_pages(&objects).and_then(|id| objects.get(&id)) else {
        return String::new();
    };
    let kids = references_in(&pages.dict, "/Kids");
    let mut out = String::new();
    for (index, kid) in kids.iter().enumerate() {
        if index > 0 {
            out.push('\u{c}');
        }
        let Some(page) = objects.get(kid) else {
            continue;
        };
        let fonts = page_fonts(&objects, &page.dict);
        let Some(content) = reference_after(&page.dict, "/Contents ")
            .and_then(|id| objects.get(&id))
            .and_then(|object| object.stream.as_ref())
        else {
            continue;
        };
        out.push_str(&page_text(content, &fonts));
    }
    out
}

/// Resource name (`F1`) to that font's glyph → text map.
fn page_fonts(
    objects: &BTreeMap<usize, Object>,
    page: &str,
) -> BTreeMap<String, BTreeMap<u16, String>> {
    let resources = reference_after(page, "/Resources ")
        .and_then(|id| objects.get(&id))
        .map(|object| object.dict.clone())
        .unwrap_or_default();
    let mut fonts = BTreeMap::new();
    let Some(start) = resources.find("/Font <<") else {
        return fonts;
    };
    let body = &resources[start + "/Font <<".len()..];
    let body = &body[..body.find(">>").unwrap_or(body.len())];
    let tokens: Vec<&str> = body.split_whitespace().collect();
    for window in tokens.windows(4) {
        if let [name, id, "0", "R"] = window
            && let Some(name) = name.strip_prefix('/')
            && let Ok(id) = id.parse::<usize>()
        {
            let map = objects
                .get(&id)
                .and_then(|font| reference_after(&font.dict, "/ToUnicode "))
                .and_then(|id| objects.get(&id))
                .and_then(|cmap| cmap.stream.as_ref())
                .map(|cmap| to_unicode(&String::from_utf8_lossy(cmap)))
                .unwrap_or_default();
            fonts.insert(name.to_owned(), map);
        }
    }
    fonts
}

/// `bfchar` entries of a ToUnicode CMap.
fn to_unicode(cmap: &str) -> BTreeMap<u16, String> {
    let mut map = BTreeMap::new();
    let mut inside = false;
    for line in cmap.lines() {
        let line = line.trim();
        if line.ends_with("beginbfchar") {
            inside = true;
            continue;
        }
        if line == "endbfchar" {
            inside = false;
            continue;
        }
        if !inside {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(glyph), Some(text)) = (parts.next(), parts.next()) else {
            continue;
        };
        let glyph = hex_bytes(glyph.trim_matches(|c| c == '<' || c == '>'));
        let text = hex_bytes(text.trim_matches(|c| c == '<' || c == '>'));
        if glyph.len() != 2 {
            continue;
        }
        let units: Vec<u16> = text
            .chunks(2)
            .map(|pair| u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]))
            .collect();
        map.insert(
            u16::from_be_bytes([glyph[0], glyph[1]]),
            String::from_utf16_lossy(&units),
        );
    }
    map
}

/// Interpret `Tf`, `Tm` and `Tj`: a new line whenever the baseline moves.
fn page_text(content: &[u8], fonts: &BTreeMap<String, BTreeMap<u16, String>>) -> String {
    let text = String::from_utf8_lossy(content);
    let mut out = String::new();
    let mut operands: Vec<&str> = Vec::new();
    let mut font: Option<&BTreeMap<u16, String>> = None;
    let mut baseline: Option<String> = None;
    for token in tokens(&text) {
        match token {
            "Tf" => {
                let name = operands
                    .iter()
                    .rev()
                    .find_map(|operand| operand.strip_prefix('/'));
                font = name.and_then(|name| fonts.get(name));
            }
            "Tm" => {
                let y = operands.last().map(|y| (*y).to_owned());
                if baseline.is_some() && baseline != y {
                    out.push('\n');
                }
                baseline = y;
            }
            "Tj" => {
                if let (Some(hex), Some(map)) = (operands.last(), font) {
                    let bytes = hex_bytes(hex.trim_matches(|c| c == '<' || c == '>'));
                    for pair in bytes.chunks(2) {
                        let glyph = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
                        out.push_str(map.get(&glyph).map_or("\u{fffd}", String::as_str));
                    }
                }
            }
            _ => {}
        }
        if token.chars().all(|c| c.is_ascii_alphabetic() || c == '*') && !token.is_empty() {
            operands.clear();
        } else {
            operands.push(token);
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Whitespace-separated tokens, keeping `<…>` hex strings whole.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let start = at;
        if bytes[at] == b'<' {
            while at < bytes.len() && bytes[at] != b'>' {
                at += 1;
            }
            at += 1;
        } else {
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'<' {
                at += 1;
            }
        }
        out.push(&text[start..at.min(bytes.len())]);
    }
    out
}

fn objects(pdf: &[u8]) -> BTreeMap<usize, Object> {
    let mut out = BTreeMap::new();
    let mut at = 0;
    while let Some(found) = find(&pdf[at..], b" 0 obj\n") {
        let header_end = at + found;
        let line_start = pdf[..header_end]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1);
        let body_start = header_end + b" 0 obj\n".len();
        at = body_start;
        let Ok(number) = std::str::from_utf8(&pdf[line_start..header_end])
            .unwrap_or("")
            .parse::<usize>()
        else {
            continue;
        };
        // A dictionary never contains either marker; the first one decides.
        let body = &pdf[body_start..];
        let stream_at = find(body, b"\nstream\n");
        let end_at = find(body, b"\nendobj\n");
        match (stream_at, end_at) {
            (Some(split), end) if end.is_none_or(|end| split < end) => {
                let dict = String::from_utf8_lossy(&body[..split]).into_owned();
                let data_start = split + b"\nstream\n".len();
                let length = number_after(&dict, "/Length ").unwrap_or(0);
                let Some(raw) = body.get(data_start..data_start + length) else {
                    break;
                };
                let data = if dict.contains("/FlateDecode") {
                    miniz_oxide::inflate::decompress_to_vec_zlib(raw).unwrap_or_default()
                } else {
                    raw.to_vec()
                };
                out.insert(
                    number,
                    Object {
                        dict,
                        stream: Some(data),
                    },
                );
                at = body_start + data_start + length;
            }
            (_, Some(end)) => {
                let dict = String::from_utf8_lossy(&body[..end]).into_owned();
                out.insert(number, Object { dict, stream: None });
                at = body_start + end;
            }
            _ => break,
        }
    }
    out
}

fn catalog_pages(objects: &BTreeMap<usize, Object>) -> Option<usize> {
    objects
        .values()
        .find(|object| object.dict.contains("/Type /Catalog"))
        .and_then(|catalog| reference_after(&catalog.dict, "/Pages "))
}

fn number_after(text: &str, key: &str) -> Option<usize> {
    let at = text.find(key)? + key.len();
    text[at..].split_whitespace().next()?.parse().ok()
}

fn reference_after(text: &str, key: &str) -> Option<usize> {
    let at = text.find(key)? + key.len();
    let mut parts = text[at..].split_whitespace();
    let id = parts.next()?.parse().ok()?;
    (parts.next() == Some("0") && parts.next()?.starts_with('R')).then_some(id)
}

fn references_in(text: &str, key: &str) -> Vec<usize> {
    let Some(at) = text.find(key) else {
        return vec![];
    };
    let rest = &text[at + key.len()..];
    let Some(open) = rest.find('[') else {
        return vec![];
    };
    let close = rest.find(']').unwrap_or(rest.len());
    let tokens: Vec<&str> = rest[open + 1..close].split_whitespace().collect();
    tokens
        .chunks(3)
        .filter_map(|chunk| chunk.first()?.parse().ok())
        .collect()
}

/// Decode a literal string body (after its opening parenthesis).
fn literal_string(rest: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut chars = rest.bytes();
    while let Some(c) = chars.next() {
        match c {
            b'\\' => match chars.next()? {
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                other => out.push(other),
            },
            b'(' => {
                depth += 1;
                out.push(c);
            }
            b')' if depth == 0 => return String::from_utf8(out).ok(),
            b')' => {
                depth -= 1;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    None
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    let digits: Vec<u8> = hex
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            _ => b - b'A' + 10,
        })
        .collect();
    digits
        .chunks(2)
        .map(|pair| pair[0] << 4 | pair.get(1).copied().unwrap_or(0))
        .collect()
}

fn line_at(pdf: &[u8], at: usize) -> String {
    let end = pdf[at..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(pdf.len(), |p| at + p);
    String::from_utf8_lossy(&pdf[at..end]).into_owned()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}
