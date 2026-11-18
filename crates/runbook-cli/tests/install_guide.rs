//! D2: every `runbook` command in the clean-install guide and the pilot
//! scripts is a real command with real flags.
//!
//! Each invocation is extracted from the ```sh blocks of `docs/install.md`
//! and `examples/pilot/README.md`, and from the `rb <who> …`,
//! `person <who> …`, `agent …` and `"$RUNBOOK_BIN" …` calls of
//! `examples/pilot/*.sh`, then run as `runbook <args> --help`. Clap parses
//! every argument before it reaches `--help`, so an unknown subcommand or
//! flag, or an invalid enum value, fails here; no request is sent.
use std::path::Path;
use std::process::{Command, Stdio};

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Shell words (quotes removed), stopping at an unquoted pipe,
/// redirection, separator or closing parenthesis.
fn words(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, '>' | '<') => {
                // `2> /dev/null`: the digit names a descriptor, not an argument.
                if word.chars().all(|c| c.is_ascii_digit()) {
                    word.clear();
                }
                break;
            }
            (None, '|' | ';' | '&' | ')') => break,
            (None, c) if c.is_whitespace() => {
                if started || !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                started = false;
            }
            (None, c) => word.push(c),
        }
    }
    if started || !word.is_empty() {
        out.push(word);
    }
    out
}

/// Lines with backslash continuations joined; `only_sh_blocks` keeps the
/// lines of ```sh blocks of a Markdown file.
fn joined_lines(text: &str, only_sh_blocks: bool) -> Vec<String> {
    let mut lines = vec![];
    let mut in_block = !only_sh_blocks;
    let mut pending = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if only_sh_blocks && trimmed.starts_with("```") {
            in_block = !in_block && matches!(trimmed, "```sh" | "```bash" | "```shell");
            continue;
        }
        if !in_block || trimmed.starts_with('#') {
            continue;
        }
        if let Some(head) = line.strip_suffix('\\') {
            pending.push_str(head);
            pending.push(' ');
            continue;
        }
        pending.push_str(line);
        lines.push(std::mem::take(&mut pending));
    }
    lines
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// Arguments of a `runbook` command written at the start of a line, after
/// `VAR=…` assignments or inside `VAR=$(`.
fn guide_invocation(line: &str) -> Option<Vec<String>> {
    let mut text = line.trim_start();
    if let Some((name, rest)) = text.split_once("=$(")
        && is_assignment(&format!("{name}="))
        && !name.contains(' ')
    {
        text = rest;
    }
    let all = words(text);
    let start = all.iter().position(|w| !is_assignment(w))?;
    (all[start] == "runbook").then(|| all[start + 1..].to_vec())
}

/// Arguments of the CLI calls the pilot scripts make through their
/// wrappers (`rb <who>`, `person <who>`, `agent`, `"$RUNBOOK_BIN"`).
fn script_invocations(line: &str) -> Vec<Vec<String>> {
    let mut found = vec![];
    for (marker, skip_who) in [
        ("rb ", true),
        ("person ", true),
        ("agent ", false),
        ("\"$RUNBOOK_BIN\" ", false),
    ] {
        let mut from = 0;
        while let Some(at) = line[from..].find(marker).map(|i| i + from) {
            from = at + marker.len();
            let before = line[..at].chars().last();
            if !matches!(before, None | Some(' ' | '(' | '\t')) {
                continue;
            }
            let mut args = words(&line[from..]);
            if skip_who && !args.is_empty() {
                args.remove(0);
            }
            if marker == "agent "
                && !matches!(
                    args.first().map(String::as_str),
                    Some("step" | "whoami" | "run")
                )
            {
                continue;
            }
            let args: Vec<String> = args
                .into_iter()
                .flat_map(|w| match w.as_str() {
                    "$@" => vec![],
                    "${events[@]}" => vec!["--event".into(), "agent.work_available".into()],
                    "$verdict" => vec!["pass".into()],
                    _ => vec![w],
                })
                .collect();
            if args
                .first()
                .is_some_and(|a| a.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
            {
                found.push(args);
            }
        }
    }
    found
}

/// Run `runbook <args> --help` in an empty home; return the parse error.
/// (The CLI prints help to stderr and exits 2, so success is help text
/// rather than an `error:` line.)
fn check(args: &[String]) -> Option<String> {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_runbook"))
        .args(args)
        .arg("--help")
        .env_remove("RUNBOOK_TOKEN")
        .env_remove("RUNBOOK_HOST")
        .env("XDG_CONFIG_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let help = (stdout.contains("Usage: runbook") || stderr.contains("Usage: runbook"))
        && !stderr.trim_start().starts_with("error:");
    (!help).then_some(stderr.into_owned())
}

fn assert_all_parse(source: &str, invocations: Vec<Vec<String>>, at_least: usize) {
    assert!(
        invocations.len() >= at_least,
        "{source}: found only {} runbook commands",
        invocations.len()
    );
    let failures: Vec<String> = invocations
        .iter()
        .filter_map(|args| {
            check(args).map(|e| format!("runbook {}\n  {}", args.join(" "), e.trim()))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{source}: commands the CLI does not accept:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_runbook_command_in_the_install_guide_parses() {
    for (file, at_least) in [("docs/install.md", 50), ("examples/pilot/README.md", 0)] {
        let text = std::fs::read_to_string(root().join(file)).expect(file);
        let invocations: Vec<Vec<String>> = joined_lines(&text, true)
            .iter()
            .filter_map(|l| guide_invocation(l))
            .collect();
        assert_all_parse(file, invocations, at_least);
    }
}

#[test]
fn every_runbook_command_in_the_pilot_scripts_parses() {
    let mut total = 0;
    for script in ["examples/pilot/setup.sh", "examples/pilot/run-flagship.sh"] {
        let text = std::fs::read_to_string(root().join(script)).expect(script);
        let invocations: Vec<Vec<String>> = joined_lines(&text, false)
            .iter()
            .flat_map(|l| script_invocations(l))
            .collect();
        total += invocations.len();
        assert_all_parse(script, invocations, 5);
    }
    assert!(total >= 30, "found {total} script commands");
}

#[test]
fn the_extractor_sees_through_assignments_and_pipes() {
    assert_eq!(
        guide_invocation(
            r#"EXE=$(runbook run start post-deploy --project pilot --json | pj get id)"#
        ),
        Some(
            vec![
                "run",
                "start",
                "post-deploy",
                "--project",
                "pilot",
                "--json"
            ]
            .into_iter()
            .map(String::from)
            .collect()
        )
    );
    assert_eq!(
        guide_invocation(
            r#"RUNBOOK_TOKEN="$(cat "$PILOT_DIR/launcher.svc")" runbook run token "$EXE" --session agent-1"#
        ),
        Some(
            vec!["run", "token", "$EXE", "--session", "agent-1"]
                .into_iter()
                .map(String::from)
                .collect()
        )
    );
    assert_eq!(
        guide_invocation("cargo build -p runbook-server -p runbook-cli"),
        None
    );
    assert_eq!(
        script_invocations(
            r#"  id=$(rb owner webhook list --project "$PILOT_PROJECT" --json | pj find items x id || true)"#
        ),
        vec![
            vec!["webhook", "list", "--project", "$PILOT_PROJECT", "--json"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        ]
    );
    assert!(script_invocations(r#"    if [ "$actor" = agent ]; then"#).is_empty());
    // A bad flag is caught.
    assert!(
        check(&[
            "step".into(),
            "report".into(),
            "sr_x".into(),
            "--status".into(),
            "done".into()
        ])
        .is_some()
    );
    assert!(check(&["webhook".into(), "create".into(), "--no-such-flag".into()]).is_some());
}
