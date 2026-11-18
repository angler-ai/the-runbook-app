//! Markdown events to positioned drawing operations on A4 pages.
//!
//! The layout is deliberately plain: one column, greedy line breaking on
//! spaces (a word wider than the line breaks between characters), headings
//! kept with what follows them, and tables sized to their content within the
//! text width. Coordinates are PDF points with the origin at the bottom left.
use crate::fonts::{Family, FontId, Fonts};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;

pub const PAGE_WIDTH: f64 = 595.28;
pub const PAGE_HEIGHT: f64 = 841.89;
const MARGIN_X: f64 = 56.0;
const TOP: f64 = PAGE_HEIGHT - 56.0;
const BOTTOM: f64 = 64.0;
const TEXT_WIDTH: f64 = PAGE_WIDTH - 2.0 * MARGIN_X;

const BODY: f64 = 10.0;
const CODE: f64 = 8.5;
const TABLE: f64 = 9.0;
const FOOTER: f64 = 8.0;
const LEADING: f64 = 1.45;
const CELL_PAD: f64 = 4.0;
const LIST_INDENT: f64 = 18.0;
const QUOTE_INDENT: f64 = 12.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color(pub u8, pub u8, pub u8);
const INK: Color = Color(0x1a, 0x1a, 0x1a);
const MUTED: Color = Color(0x55, 0x5b, 0x65);
const LINK: Color = Color(0x1f, 0x4f, 0xbf);
const RULE: Color = Color(0xc8, 0xcc, 0xd2);
const CODE_BG: Color = Color(0xf3, 0xf4, 0xf6);
const HEAD_BG: Color = Color(0xea, 0xec, 0xf0);

/// A run of glyphs in one font.
pub struct Run {
    pub font: FontId,
    pub glyphs: Vec<u16>,
}

pub enum Op {
    Text {
        x: f64,
        y: f64,
        size: f64,
        italic: bool,
        color: Color,
        runs: Vec<Run>,
    },
    Fill {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        color: Color,
    },
    Stroke {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        color: Color,
    },
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        width: f64,
        color: Color,
    },
}

pub struct Document {
    pub title: String,
    pub pages: Vec<Vec<Op>>,
    /// Per font, every glyph drawn: the character it stands for and its
    /// width in glyph space (for `/W` and the ToUnicode map).
    pub used: BTreeMap<FontId, BTreeMap<u16, (char, i32)>>,
    pub fonts: Fonts,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Style {
    family: Family,
    size: f64,
    italic: bool,
    color: Color,
}

impl Style {
    fn body() -> Self {
        Style {
            family: Family::Sans,
            size: BODY,
            italic: false,
            color: INK,
        }
    }
}

#[derive(Clone, Debug)]
struct Span {
    text: String,
    style: Style,
}

/// One laid-out line: fragments at x offsets from the line's left edge.
struct Line {
    frags: Vec<(f64, Style, String)>,
    height: f64,
    size: f64,
}

struct Table {
    rows: Vec<(bool, Vec<Vec<Span>>)>,
    row: Vec<Vec<Span>>,
    head: bool,
}

pub fn lay_out(markdown: &str) -> Document {
    let mut builder = Builder::new();
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        builder.event(event);
    }
    builder.finish()
}

struct Builder {
    composer: Composer,
    title: Option<String>,
    spans: Vec<Span>,
    bold: usize,
    italic: usize,
    links: Vec<(String, usize)>,
    heading: Option<HeadingLevel>,
    lists: Vec<Option<u64>>,
    marker: Option<String>,
    quote: usize,
    code: Option<String>,
    table: Option<Table>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            composer: Composer::new(),
            title: None,
            spans: Vec::new(),
            bold: 0,
            italic: 0,
            links: Vec::new(),
            heading: None,
            lists: Vec::new(),
            marker: None,
            quote: 0,
            code: None,
            table: None,
        }
    }

    fn style(&self) -> Style {
        let mut style = Style::body();
        if let Some(level) = self.heading {
            style.family = Family::SansBold;
            style.size = heading_size(level);
        } else if self.table.as_ref().is_some_and(|t| t.head) {
            style.family = Family::SansBold;
            style.size = TABLE;
        } else if self.table.is_some() {
            style.size = TABLE;
        }
        if self.bold > 0 {
            style.family = Family::SansBold;
        }
        style.italic = self.italic > 0;
        if !self.links.is_empty() {
            style.color = LINK;
        } else if self.quote > 0 {
            style.color = MUTED;
        }
        style
    }

    fn push(&mut self, text: &str, style: Style) {
        if let Some(code) = &mut self.code {
            code.push_str(text);
            return;
        }
        self.spans.push(Span {
            text: text.to_owned(),
            style,
        });
    }

    fn left(&self) -> f64 {
        MARGIN_X + self.quote as f64 * QUOTE_INDENT + self.lists.len() as f64 * LIST_INDENT
    }

    /// Lay out the pending inline text as a paragraph (or list item text).
    fn flush(&mut self) {
        let spans = std::mem::take(&mut self.spans);
        let marker = self.marker.take();
        if spans.iter().all(|s| s.text.trim().is_empty()) && marker.is_none() {
            return;
        }
        let left = self.left();
        let width = MARGIN_X + TEXT_WIDTH - left;
        let lines = self.composer.wrap(&spans, width);
        let quote = self.quote;
        for (index, line) in lines.iter().enumerate() {
            self.composer.ensure(line.height);
            if index == 0
                && let Some(marker) = &marker
            {
                let style = Style {
                    color: MUTED,
                    ..Style::body()
                };
                self.composer.text_at(left - LIST_INDENT + 4.0, line, |_| {
                    vec![(0.0, style, marker.clone())]
                });
            }
            if quote > 0 {
                let y = self.composer.y;
                for depth in 0..quote {
                    let x = MARGIN_X + depth as f64 * QUOTE_INDENT + 3.0;
                    self.composer.op(Op::Line {
                        x1: x,
                        y1: y,
                        x2: x,
                        y2: y - line.height,
                        width: 1.5,
                        color: RULE,
                    });
                }
            }
            self.composer.draw_line(left, line);
        }
        let gap = if self.lists.is_empty() { 6.0 } else { 2.0 };
        self.composer.gap(gap);
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                let style = self.style();
                self.push(&text, style);
            }
            Event::Code(text) => {
                let style = Style {
                    family: Family::Mono,
                    size: self.style().size * 0.92,
                    ..self.style()
                };
                self.push(&text, style);
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                // Shown as text, never interpreted.
                let style = self.style();
                self.push(&text, style);
            }
            Event::SoftBreak => {
                let style = self.style();
                self.push(" ", style);
            }
            Event::HardBreak => {
                let style = self.style();
                self.push("\n", style);
            }
            Event::Rule => {
                self.flush();
                self.composer.ensure(12.0);
                let y = self.composer.y - 6.0;
                self.composer.op(Op::Line {
                    x1: MARGIN_X,
                    y1: y,
                    x2: MARGIN_X + TEXT_WIDTH,
                    y2: y,
                    width: 0.75,
                    color: RULE,
                });
                self.composer.gap(12.0);
            }
            Event::TaskListMarker(done) => {
                let style = self.style();
                self.push(if done { "[x] " } else { "[ ] " }, style);
            }
            Event::FootnoteReference(name) => {
                let style = self.style();
                self.push(&format!("[{name}]"), style);
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                let style = self.style();
                self.push(&text, style);
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(next)) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    _ => "\u{2022} ".to_owned(),
                };
                self.marker = Some(marker);
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(Table {
                    rows: Vec::new(),
                    row: Vec::new(),
                    head: false,
                });
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.head = true;
                }
            }
            Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Link { dest_url, .. } => {
                self.links.push((dest_url.to_string(), self.spans.len()));
            }
            Tag::Image { .. } => {
                let style = self.style();
                self.push("[image: ", style);
            }
            Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => self.flush(),
            TagEnd::Heading(level) => {
                let spans = std::mem::take(&mut self.spans);
                self.heading = None;
                let text: String = spans.iter().map(|s| s.text.as_str()).collect();
                if self.title.is_none() && level == HeadingLevel::H1 {
                    self.title = Some(text);
                }
                self.composer.heading(&spans, level);
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                let left = self.left();
                self.composer.code_block(&code, left);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.composer.gap(4.0);
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::TableCell => {
                let spans = std::mem::take(&mut self.spans);
                if let Some(table) = &mut self.table {
                    table.row.push(spans);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push((table.head, row));
                    table.head = false;
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let left = self.left();
                    self.composer.table(&table.rows, left);
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Link => {
                if let Some((url, from)) = self.links.pop() {
                    let text: String = self.spans[from.min(self.spans.len())..]
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect();
                    if !url.is_empty() && text != url {
                        let style = Style {
                            color: MUTED,
                            ..self.style()
                        };
                        self.push(&format!(" ({url})"), style);
                    }
                }
            }
            TagEnd::Image => {
                let style = self.style();
                self.push("]", style);
            }
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn finish(mut self) -> Document {
        self.flush();
        let mut composer = self.composer;
        composer.footers();
        Document {
            title: self.title.unwrap_or_default(),
            pages: composer.pages,
            used: composer.used,
            fonts: composer.fonts,
        }
    }
}

fn heading_size(level: HeadingLevel) -> f64 {
    match level {
        HeadingLevel::H1 => 18.0,
        HeadingLevel::H2 => 13.0,
        HeadingLevel::H3 => 11.5,
        _ => 10.5,
    }
}

/// Pages, the cursor, and the glyphs used so far.
struct Composer {
    fonts: Fonts,
    pages: Vec<Vec<Op>>,
    y: f64,
    used: BTreeMap<FontId, BTreeMap<u16, (char, i32)>>,
}

impl Composer {
    fn new() -> Self {
        Composer {
            fonts: Fonts::load(),
            pages: vec![Vec::new()],
            y: TOP,
            used: BTreeMap::new(),
        }
    }

    fn op(&mut self, op: Op) {
        if let Some(page) = self.pages.last_mut() {
            page.push(op);
        }
    }

    fn at_top(&self) -> bool {
        self.y >= TOP
    }

    /// Start a new page unless `height` still fits on this one.
    fn ensure(&mut self, height: f64) {
        if self.y - height < BOTTOM && !self.at_top() {
            self.pages.push(Vec::new());
            self.y = TOP;
        }
    }

    /// Vertical space, dropped at the top of a page.
    fn gap(&mut self, height: f64) {
        if !self.at_top() {
            self.y -= height;
        }
    }

    fn char_width(&self, family: Family, size: f64, c: char) -> f64 {
        let (font, glyph) = self.fonts.glyph(family, c);
        f64::from(self.fonts.width(font, glyph)) * size / 1000.0
    }

    fn text_width(&self, text: &str, style: Style) -> f64 {
        text.chars()
            .map(|c| self.char_width(style.family, style.size, c))
            .sum()
    }

    /// Greedy line breaking: spaces are break points and are dropped at the
    /// ends of lines; `\n` forces a break; a token wider than the line
    /// breaks between characters.
    fn wrap(&self, spans: &[Span], width: f64) -> Vec<Line> {
        let mut lines = Vec::new();
        let mut line: Vec<(Style, String, f64)> = Vec::new();
        let mut line_width = 0.0;
        let finish = |line: &mut Vec<(Style, String, f64)>, lines: &mut Vec<Line>| {
            while line.last().is_some_and(|(_, t, _)| t.trim().is_empty()) {
                line.pop();
            }
            lines.push(self.line(std::mem::take(line)));
        };
        for span in spans {
            for token in tokens(&span.text) {
                if token == "\n" {
                    finish(&mut line, &mut lines);
                    line_width = 0.0;
                    continue;
                }
                let space = token.trim().is_empty();
                if space && line.is_empty() {
                    continue;
                }
                let token_width = self.text_width(token, span.style);
                if space || line_width + token_width <= width {
                    line.push((span.style, token.to_owned(), token_width));
                    line_width += token_width;
                    continue;
                }
                if !line.is_empty() {
                    finish(&mut line, &mut lines);
                }
                // On a fresh line; a token wider than the line breaks
                // between characters.
                let mut chunk = String::new();
                let mut chunk_width = 0.0;
                for c in token.chars() {
                    let w = self.char_width(span.style.family, span.style.size, c);
                    if chunk_width + w > width && !chunk.is_empty() {
                        line.push((span.style, std::mem::take(&mut chunk), chunk_width));
                        finish(&mut line, &mut lines);
                        chunk_width = 0.0;
                    }
                    chunk.push(c);
                    chunk_width += w;
                }
                line_width = chunk_width;
                line.push((span.style, chunk, chunk_width));
            }
        }
        if !line.is_empty() || lines.is_empty() {
            finish(&mut line, &mut lines);
        }
        lines
    }

    fn line(&self, pieces: Vec<(Style, String, f64)>) -> Line {
        let size = pieces
            .iter()
            .map(|(style, _, _)| style.size)
            .fold(0.0, f64::max);
        let size = if size > 0.0 { size } else { BODY };
        let mut frags: Vec<(f64, Style, String)> = Vec::new();
        let mut x = 0.0;
        for (style, text, width) in pieces {
            match frags.last_mut() {
                Some((_, last, merged)) if *last == style => merged.push_str(&text),
                _ => frags.push((x, style, text)),
            }
            x += width;
        }
        Line {
            frags,
            height: size * LEADING,
            size,
        }
    }

    /// Draw a line at `left` on the current cursor and move below it.
    fn draw_line(&mut self, left: f64, line: &Line) {
        self.text_at(left, line, |line| line.frags.clone());
        self.y -= line.height;
    }

    /// Draw fragments on the baseline of `line` at the cursor.
    fn text_at(
        &mut self,
        left: f64,
        line: &Line,
        frags: impl Fn(&Line) -> Vec<(f64, Style, String)>,
    ) {
        let baseline = self.y - (line.height - line.size) / 2.0 - line.size * 0.8;
        for (x, style, text) in frags(line) {
            let runs = self.runs(&text, style.family);
            if runs.is_empty() {
                continue;
            }
            self.op(Op::Text {
                x: left + x,
                y: baseline,
                size: style.size,
                italic: style.italic,
                color: style.color,
                runs,
            });
        }
    }

    /// Glyph runs for `text`, recording each glyph used.
    fn runs(&mut self, text: &str, family: Family) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();
        for c in text.chars() {
            if c.is_control() {
                continue;
            }
            let (font, glyph) = self.fonts.glyph(family, c);
            // A character no font covers is drawn, and read back, as `?`.
            let shown = if c == '?' || has_glyph(&self.fonts, family, c) {
                c
            } else {
                '?'
            };
            let width = self.fonts.width(font, glyph);
            self.used
                .entry(font)
                .or_default()
                .entry(glyph)
                .or_insert((shown, width));
            match runs.last_mut() {
                Some(run) if run.font == font => run.glyphs.push(glyph),
                _ => runs.push(Run {
                    font,
                    glyphs: vec![glyph],
                }),
            }
        }
        runs
    }

    fn heading(&mut self, spans: &[Span], level: HeadingLevel) {
        let before = match level {
            HeadingLevel::H1 => 4.0,
            HeadingLevel::H2 => 12.0,
            _ => 8.0,
        };
        self.gap(before);
        let lines = self.wrap(spans, TEXT_WIDTH);
        let height: f64 = lines.iter().map(|l| l.height).sum();
        // Keep the heading with at least three body lines.
        self.ensure(height + 3.0 * BODY * LEADING);
        for line in &lines {
            self.draw_line(MARGIN_X, line);
        }
        if level == HeadingLevel::H1 {
            let y = self.y - 2.0;
            self.op(Op::Line {
                x1: MARGIN_X,
                y1: y,
                x2: MARGIN_X + TEXT_WIDTH,
                y2: y,
                width: 0.75,
                color: RULE,
            });
            self.y -= 8.0;
        } else {
            self.y -= 3.0;
        }
    }

    fn code_block(&mut self, code: &str, left: f64) {
        let style = Style {
            family: Family::Mono,
            size: CODE,
            italic: false,
            color: INK,
        };
        let pad = 5.0;
        let width = MARGIN_X + TEXT_WIDTH - left;
        let code = code
            .strip_suffix('\n')
            .unwrap_or(code)
            .replace('\t', "    ");
        let mut lines = Vec::new();
        for source in code.split('\n') {
            lines.extend(self.wrap_code(source, style, width - 2.0 * pad));
        }
        let first = lines.first().map_or(0.0, |l| l.height);
        self.ensure(first + 2.0 * pad);
        self.op(Op::Fill {
            x: left,
            y: self.y - pad,
            w: width,
            h: pad,
            color: CODE_BG,
        });
        self.y -= pad;
        for line in &lines {
            self.ensure(line.height);
            self.op(Op::Fill {
                x: left,
                y: self.y - line.height,
                w: width,
                h: line.height,
                color: CODE_BG,
            });
            self.draw_line(left + pad, line);
        }
        self.op(Op::Fill {
            x: left,
            y: self.y - pad,
            w: width,
            h: pad,
            color: CODE_BG,
        });
        self.y -= pad;
        self.gap(8.0);
    }

    /// Code keeps its spaces and breaks only between characters.
    fn wrap_code(&self, source: &str, style: Style, width: f64) -> Vec<Line> {
        let mut lines = Vec::new();
        let mut chunk = String::new();
        let mut chunk_width = 0.0;
        for c in source.chars() {
            let w = self.char_width(style.family, style.size, c);
            if chunk_width + w > width && !chunk.is_empty() {
                lines.push(self.line(vec![(style, std::mem::take(&mut chunk), chunk_width)]));
                chunk_width = 0.0;
            }
            chunk.push(c);
            chunk_width += w;
        }
        lines.push(self.line(vec![(style, chunk, chunk_width)]));
        lines
    }

    fn table(&mut self, rows: &[(bool, Vec<Vec<Span>>)], left: f64) {
        let columns = rows.iter().map(|(_, row)| row.len()).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let available = MARGIN_X + TEXT_WIDTH - left;
        let natural: Vec<f64> = (0..columns)
            .map(|column| {
                rows.iter()
                    .filter_map(|(_, row)| row.get(column))
                    .map(|cell| {
                        cell.iter()
                            .map(|span| self.text_width(&span.text, span.style))
                            .sum::<f64>()
                    })
                    .fold(0.0, f64::max)
                    + 2.0 * CELL_PAD
                    + 1.0
            })
            .collect();
        let widths = column_widths(&natural, available);
        for (head, row) in rows {
            let cells: Vec<Vec<Line>> = (0..columns)
                .map(|column| {
                    let spans = row.get(column).map_or(&[][..], Vec::as_slice);
                    self.wrap(spans, widths[column] - 2.0 * CELL_PAD)
                })
                .collect();
            let height = cells
                .iter()
                .map(|lines| lines.iter().map(|l| l.height).sum::<f64>())
                .fold(0.0, f64::max)
                + 2.0 * CELL_PAD;
            self.ensure(height);
            let top = self.y;
            let mut x = left;
            for (column, lines) in cells.iter().enumerate() {
                let w = widths[column];
                if *head {
                    self.op(Op::Fill {
                        x,
                        y: top - height,
                        w,
                        h: height,
                        color: HEAD_BG,
                    });
                }
                self.op(Op::Stroke {
                    x,
                    y: top - height,
                    w,
                    h: height,
                    color: RULE,
                });
                self.y = top - CELL_PAD;
                for line in lines {
                    self.draw_line(x + CELL_PAD, line);
                }
                x += w;
            }
            self.y = top - height;
        }
        self.gap(8.0);
    }

    /// `Page n of N`, right-aligned in the bottom margin of every page.
    fn footers(&mut self) {
        let total = self.pages.len();
        let style = Style {
            family: Family::Sans,
            size: FOOTER,
            italic: false,
            color: MUTED,
        };
        for index in 0..total {
            let text = format!("Page {} of {total}", index + 1);
            let width = self.text_width(&text, style);
            let runs = self.runs(&text, style.family);
            self.pages[index].push(Op::Text {
                x: MARGIN_X + TEXT_WIDTH - width,
                y: 32.0,
                size: FOOTER,
                italic: false,
                color: MUTED,
                runs,
            });
        }
    }
}

fn has_glyph(fonts: &Fonts, family: Family, c: char) -> bool {
    let (font, glyph) = fonts.glyph(family, c);
    let (q_font, q_glyph) = fonts.glyph(family, '?');
    (font, glyph) != (q_font, q_glyph)
}

/// Columns no wider than they need; when the table is too wide, narrow
/// columns keep their width and wide ones share the rest in proportion.
fn column_widths(natural: &[f64], available: f64) -> Vec<f64> {
    let total: f64 = natural.iter().sum();
    if total <= available {
        return natural.to_vec();
    }
    let fair = available / natural.len() as f64;
    let narrow: f64 = natural.iter().filter(|w| **w <= fair).sum();
    let wide: f64 = natural.iter().filter(|w| **w > fair).sum();
    let rest = available - narrow;
    natural
        .iter()
        .map(|w| if *w <= fair { *w } else { w / wide * rest })
        .collect()
}

/// Words, runs of spaces and `\n`, in order.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut space: Option<bool> = None;
    for (at, c) in text.char_indices() {
        if c == '\n' {
            if start < at {
                out.push(&text[start..at]);
            }
            out.push("\n");
            start = at + 1;
            space = None;
            continue;
        }
        let is_space = c == ' ';
        if space.is_some_and(|s| s != is_space) {
            out.push(&text[start..at]);
            start = at;
        }
        space = Some(is_space);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}
