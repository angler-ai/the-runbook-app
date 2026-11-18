//! D52 golden-structure tests: the PDF is read back with the crate's
//! independent `inspect` reader (cross-reference table, page tree,
//! ToUnicode maps), never compared to the writer's own state.
use runbook_report_pdf::{inspect, render_pdf};

const REPORT: &str = "# Deploy api v2\\.0\\.0\n\n\
Status: **succeeded**\n\n\
## Report\n\n\
Deploy api finished as *kept*.\n\n\
| Region | Error rate |\n| --- | --- |\n| eu\\-west | 0\\.001 |\n| us\\-east | 0\\.002 |\n\n\
## Health endpoint returns 200 (health)\n\n\
- Status: succeeded\n- Verdict: pass\n- Actor: Alice\n\n\
> Fixture work completed\\.\n\n\
```json\n{\n  \"status\": 200\n}\n```\n\n\
**Approval:** Keep the release?\n\n\
1. Alice chose keep\n2. Bob chose keep\n\n\
- Evidence: https://metrics\\.example\\.test/evidence\n\
[Logs](/v1/step-runs/sr_1/logs)\n\n\
---\n\n\
## Notify (notify)\n\n\
- Status: succeeded\n\n\
Inline `code` and a [link](https://example.test/x)\\.\n";

fn text(pdf: &[u8]) -> String {
    inspect::extract_text(pdf)
}

#[test]
fn report_is_a_well_formed_pdf_with_its_text() {
    let pdf = render_pdf(REPORT);
    assert!(pdf.starts_with(b"%PDF-1.7\n"), "header");
    inspect::check_structure(&pdf).expect("cross-reference table");
    assert_eq!(inspect::page_count(&pdf), 1);
    assert_eq!(
        inspect::info_title(&pdf).as_deref(),
        Some("Deploy api v2.0.0")
    );
    let text = text(&pdf);
    for expected in [
        "Deploy api v2.0.0\n",
        "Status: succeeded\n",
        "Report\n",
        "Deploy api finished as kept.\n",
        "Health endpoint returns 200 (health)\n",
        "Actor: Alice\n",
        "Fixture work completed.\n",
        "\"status\": 200\n",
        "Approval: Keep the release?\n",
        "1. Alice chose keep\n",
        // The export puts `[Logs](…)` right under the last list line, so
        // Markdown reads it as a continuation of that item.
        "\u{2022} Evidence: https://metrics.example.test/evidence Logs (/v1/step-runs/sr_1/logs)\n",
        "Notify (notify)\n",
        "Inline code and a link (https://example.test/x).\n",
        "Page 1 of 1\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    // Table cells, each on the row's baseline.
    for cell in [
        "Region",
        "Error rate",
        "eu-west",
        "0.001",
        "us-east",
        "0.002",
    ] {
        assert!(text.contains(cell), "missing cell {cell:?} in:\n{text}");
    }
    // Bullets are drawn, never data.
    assert!(text.contains("\u{2022} Status: succeeded\n"), "{text}");
}

#[test]
fn long_reports_paginate_with_page_numbers() {
    let mut markdown = String::from("# Long run\n\n");
    for i in 0..120 {
        // Sections of varied height, so headings land everywhere on a page.
        markdown.push_str(&format!("## Step number {i} (s{i})\n\n"));
        for line in 0..(i % 7) {
            markdown.push_str(&format!("- Output {line}\n"));
        }
        markdown.push_str(&format!("\n> Summary {i}\n\n"));
    }
    let pdf = render_pdf(&markdown);
    inspect::check_structure(&pdf).expect("cross-reference table");
    let pages = inspect::page_count(&pdf);
    assert!(pages > 3, "{pages} pages");
    let text = text(&pdf);
    assert_eq!(
        text.matches('\u{c}').count(),
        pages - 1,
        "one text block per page"
    );
    for i in 0..120 {
        assert!(
            text.contains(&format!("Step number {i} (s{i})\n")),
            "step {i}"
        );
    }
    assert!(
        text.contains(&format!("Page {pages} of {pages}\n")),
        "{text}"
    );
    // A heading never ends a page: the next thing on its page is its body.
    for page in text.split('\u{c}') {
        let lines: Vec<&str> = page.lines().filter(|l| !l.starts_with("Page ")).collect();
        let last = lines.last().copied().unwrap_or_default();
        assert!(!last.starts_with("Step number"), "orphan heading: {last:?}");
    }
}

#[test]
fn same_input_gives_the_same_bytes() {
    let first = render_pdf(REPORT);
    let second = render_pdf(REPORT);
    assert!(first == second, "two renders differ");
    // Nothing that varies by time or randomness is written.
    let raw = String::from_utf8_lossy(&first);
    assert!(!raw.contains("/CreationDate"), "no clock");
    assert!(!raw.contains("/ModDate"), "no clock");
    assert!(!raw.contains("/ID"), "no random id");
}

#[test]
fn untrusted_text_cannot_reach_pdf_syntax() {
    // As the Markdown export escapes it: `escape()` backslash-escapes
    // punctuation and turns `<` into `&lt;`.
    let markdown = "# Title \\(with\\) a \\\\ backslash\n\n\
> &lt;script&gt;alert\\(1\\)&lt;/script&gt; \\) Tj ET endstream endobj %%EOF \\(\n";
    let pdf = render_pdf(markdown);
    inspect::check_structure(&pdf).expect("structure survives hostile text");
    assert_eq!(
        inspect::info_title(&pdf).as_deref(),
        Some("Title (with) a \\ backslash")
    );
    let text = text(&pdf);
    assert!(text.contains("Title (with) a \\ backslash\n"), "{text}");
    assert!(
        text.contains("<script>alert(1)</script> ) Tj ET endstream endobj %%EOF (\n"),
        "{text}"
    );
    // The bytes carry glyph ids, never the characters themselves.
    let raw = String::from_utf8_lossy(&pdf);
    assert!(!raw.contains("<script>"), "raw markup in the file");
    assert!(!raw.contains("alert(1)"), "raw text in the file");
    assert_eq!(raw.matches("%%EOF").count(), 1);
}

#[test]
fn names_outside_latin_fall_back_or_show_a_placeholder() {
    let pdf = render_pdf("# Run\n\n- Actor: Łukasz Żółć\n- Actor: 漢字\n");
    inspect::check_structure(&pdf).expect("structure");
    let text = text(&pdf);
    assert!(text.contains("Actor: Łukasz Żółć\n"), "{text}");
    assert!(text.contains("Actor: ??\n"), "{text}");
}

#[test]
fn long_lines_wrap_inside_the_page() {
    let long_word = "x".repeat(400);
    let code = format!("```\n{}\n```\n", "y".repeat(400));
    let words = "word ".repeat(200);
    let pdf = render_pdf(&format!("# Wrap\n\n{long_word}\n\n{code}\n{words}\n"));
    inspect::check_structure(&pdf).expect("structure");
    let text = text(&pdf);
    let x_lines = text.lines().filter(|l| l.starts_with('x')).count();
    let y_lines = text.lines().filter(|l| l.starts_with('y')).count();
    let word_lines = text.lines().filter(|l| l.starts_with("word")).count();
    assert!(x_lines > 2, "{x_lines} lines for a long word");
    assert!(y_lines > 2, "{y_lines} lines for a long code line");
    assert!(word_lines > 5, "{word_lines} lines for a long paragraph");
    // Every character survives wrapping.
    let xs: usize = text.lines().map(|l| l.matches('x').count()).sum();
    let ys: usize = text.lines().map(|l| l.matches('y').count()).sum();
    assert_eq!((xs, ys), (400, 400));
    for line in text.lines().filter(|l| l.starts_with("word")) {
        assert!(!line.ends_with(' '), "trailing space kept: {line:?}");
    }
}

#[test]
fn only_used_fonts_are_embedded() {
    let plain = render_pdf("# Plain\n\nText.\n");
    let raw = String::from_utf8_lossy(&plain);
    assert!(raw.contains("/BaseFont /RBINTB+Inter-SemiBold"), "headings");
    assert!(raw.contains("/BaseFont /RBINTR+Inter-Regular"), "body");
    assert!(!raw.contains("JetBrainsMono"), "no code, no mono font");
    assert!(!raw.contains("RBINTX+"), "no fallback needed");
    assert_eq!(raw.matches("/FontFile2").count(), 2);
}
