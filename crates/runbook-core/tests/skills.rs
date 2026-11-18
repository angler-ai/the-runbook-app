//! B3.2a: skill bundle rules (D19). Pure; no storage.
use runbook_core::skills::*;
use sha2::{Digest, Sha256};

const SKILL_MD: &str = "---\nname: check-error-rates\ndescription: Compare error rates before and after a deploy.\n---\n# Check error rates\n";

fn file(path: &str, content: &[u8]) -> SkillFile {
    SkillFile {
        path: path.into(),
        content: content.to_vec(),
    }
}
fn bundle() -> Vec<SkillFile> {
    vec![
        file("SKILL.md", SKILL_MD.as_bytes()),
        file("references/thresholds.md", b"p99 < 250ms\n"),
        file("scripts/check.sh", b"#!/bin/sh\necho ok\n"),
    ]
}
fn build(files: &[SkillFile]) -> Result<SkillBundle, SkillError> {
    build_bundle(files, DEFAULT_MAX_BUNDLE_BYTES)
}

#[test]
fn skill_hash_covers_every_file() {
    let base = build(&bundle()).unwrap();
    assert_eq!(base.name, "check-error-rates");
    assert_eq!(
        base.description,
        "Compare error rates before and after a deploy."
    );
    assert_eq!(base.manifest.entries.len(), 3);

    let mut reversed = bundle();
    reversed.reverse();
    assert_eq!(
        build(&reversed).unwrap().hash,
        base.hash,
        "order-independent"
    );

    let mut changed = bundle();
    changed[1].content = b"p99 < 251ms\n".to_vec();
    assert_ne!(build(&changed).unwrap().hash, base.hash, "reference byte");

    let mut renamed = bundle();
    renamed[2].path = "scripts/check2.sh".into();
    assert_ne!(build(&renamed).unwrap().hash, base.hash, "path is covered");

    let mut extra = bundle();
    extra.push(file("assets/empty.png", b""));
    assert_ne!(build(&extra).unwrap().hash, base.hash, "empty file counts");

    let mut duplicate = bundle();
    duplicate.push(file("scripts/check.sh", b"other"));
    assert_eq!(build(&duplicate), Err(SkillError::DuplicatePath));
}

#[test]
fn skill_front_matter_is_required() {
    let without = |text: &str| {
        let mut files = bundle();
        files[0].content = text.as_bytes().to_vec();
        build(&files)
    };
    assert_eq!(
        build(&bundle()[1..]),
        Err(SkillError::MissingSkillFile),
        "SKILL.md must be at the root"
    );
    let nested = vec![file("docs/SKILL.md", SKILL_MD.as_bytes())];
    assert_eq!(build(&nested), Err(SkillError::MissingSkillFile));
    for (text, reason) in [
        ("# no front matter\n", "missing_opening_delimiter"),
        (
            "---\nname: x\ndescription: y\n",
            "missing_closing_delimiter",
        ),
        ("---\ndescription: y\n---\n", "missing_name"),
        ("---\nname: x\n---\n", "missing_description"),
        (
            "---\nname: x\ndescription: '  '\n---\n",
            "invalid_description",
        ),
        (
            "---\nname: Check_Rates\ndescription: y\n---\n",
            "invalid_name",
        ),
        ("---\nname: -x\ndescription: y\n---\n", "invalid_name"),
        ("---\n- a\n---\n", "not_a_mapping"),
        ("---\nname: [unclosed\n---\n", "invalid_yaml"),
    ] {
        assert_eq!(
            without(text),
            Err(SkillError::InvalidFrontMatter(reason)),
            "{text:?}"
        );
    }
    assert!(without("---\r\nname: x\r\ndescription: y\r\n---\r\nbody").is_ok());
    assert!(without("\u{feff}---\nname: x\ndescription: y\n---\n").is_ok());
    // F10: CRLF front matter with three or more lines keeps byte offsets
    // right: the last value is whole, and a multibyte ending cannot panic.
    let crlf =
        without("---\r\ndescription: y\r\nlicense: MIT\r\nname: deploy\r\n---\r\nbody").unwrap();
    assert_eq!(crlf.name, "deploy");
    let crlf =
        without("---\r\nname: deploy\r\nlicense: MIT\r\ndescription: café\r\n---\r\n").unwrap();
    assert_eq!(crlf.description, "café");
}

#[test]
fn skill_paths_cannot_escape_manifest() {
    for (path, rule) in [
        ("", "empty"),
        ("/etc/passwd", "absolute"),
        ("../secrets", "dot_component"),
        ("scripts/../../x", "dot_component"),
        ("./SKILL.md", "dot_component"),
        ("scripts//check.sh", "empty_component"),
        ("scripts/", "empty_component"),
        ("scripts\\check.sh", "backslash"),
        ("scripts/check\0.sh", "control_character"),
        ("scripts/\nx", "control_character"),
    ] {
        assert_eq!(
            validate_path(path),
            Err(SkillError::InvalidPath(rule)),
            "{path:?}"
        );
        let mut files = bundle();
        files.push(file(path, b"x"));
        assert_eq!(
            build(&files),
            Err(SkillError::InvalidPath(rule)),
            "{path:?}"
        );
    }
    assert_eq!(
        validate_path(&"a".repeat(MAX_PATH_BYTES + 1)),
        Err(SkillError::InvalidPath("too_long"))
    );
    assert!(validate_path(&"a".repeat(MAX_PATH_BYTES)).is_ok());
    assert!(validate_path("assets/diagramme-é.png").is_ok());

    let manifest = build(&bundle()).unwrap().manifest;
    assert!(manifest.contains("scripts/check.sh"));
    for absent in ["scripts", "scripts/check.sh/", "../SKILL.md", "SKILL.MD"] {
        assert!(!manifest.contains(absent), "{absent:?}");
    }
}

#[test]
fn skill_bundle_size_is_limited() {
    let files = bundle();
    let total: u64 = files.iter().map(|f| f.content.len() as u64).sum();
    assert!(build_bundle(&files, total).is_ok());
    assert_eq!(build_bundle(&files, total - 1), Err(SkillError::TooLarge));
}

/// Pins the D19 framing so the CLI and the Workers adapter produce the same hash.
/// The expected value is recomputed here from the documented rule, not taken
/// from the implementation, and checked against a fixed digest.
#[test]
fn skill_hash_framing_is_pinned() {
    let files = [
        file("SKILL.md", b"---\nname: a\ndescription: b\n---\n"),
        file("x/y.txt", b"hi"),
    ];
    let built = build(&files).unwrap();
    let digest = |bytes: &[u8]| {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let expected = format!(
        "SKILL.md\0{}\0{}\nx/y.txt\0{}\0{}\n",
        files[0].content.len(),
        digest(&files[0].content),
        files[1].content.len(),
        digest(&files[1].content),
    );
    assert_eq!(built.manifest.framing(), expected.as_bytes());
    assert_eq!(digest(expected.as_bytes()), digest_of(&built.hash));
    // Computed independently (Python hashlib) from the framing in D19.
    assert_eq!(
        digest_of(&built.hash),
        "36c316810adc9715446e3cd93e09a2ae9b5bfe2f5031ddabbc54040a9da58c66"
    );
}
fn digest_of(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
