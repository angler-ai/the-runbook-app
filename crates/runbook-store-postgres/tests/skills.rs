//! B3.2b: project skill library storage, resolution and pinned reads.
mod support;
use runbook_core::{
    StoreError,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, SkillProvenance},
    publish::{prepare, publish_draft},
    skill_library::{
        PublishSkillError, publish_skill, read_pinned_file, resolve_skill_pins, skill_usage,
    },
    skills::{DEFAULT_MAX_BUNDLE_BYTES, SkillError, SkillFile},
};
use runbook_model::{RunbookId, VersionId};
use runbook_store_conformance::scope;
use support::factory::with_factory;

fn file(path: &str, content: &[u8]) -> SkillFile {
    SkillFile {
        path: path.into(),
        content: content.to_vec(),
    }
}
fn skill_md(name: &str, body: &str) -> Vec<u8> {
    format!("---\nname: {name}\ndescription: Checks things.\n---\n{body}\n").into_bytes()
}
fn bundle(body: &str, script: &[u8]) -> Vec<SkillFile> {
    vec![
        file("SKILL.md", &skill_md("check-error-rates", body)),
        file("scripts/run.sh", script),
        file("references/guide.md", b"guide"),
    ]
}
fn provenance(reference: &str) -> SkillProvenance {
    SkillProvenance {
        reference: Some(reference.into()),
        label: "test upload".into(),
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn skill_hash_covers_every_file() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        let first = publish_skill(
            &store,
            &store,
            &bundle("v1", b"echo 1"),
            provenance("./skills/check-error-rates"),
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .await
        .unwrap();
        // Upload order never changes identity or the recorded version.
        let mut shuffled = bundle("v1", b"echo 1");
        shuffled.reverse();
        let again = publish_skill(
            &store,
            &store,
            &shuffled,
            provenance("./skills/check-error-rates"),
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(again, first);
        // One changed script byte is a different version, and becomes latest.
        let changed = publish_skill(
            &store,
            &store,
            &bundle("v1", b"echo 2"),
            provenance("./skills/check-error-rates"),
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .await
        .unwrap();
        assert_ne!(changed.hash, first.hash);
        assert_eq!(store.latest_skills().await?, vec![changed.clone()]);
        // Missing front-matter fields and duplicate paths publish nothing.
        let no_description = vec![file("SKILL.md", b"---\nname: check-error-rates\n---\nx\n")];
        assert!(matches!(
            publish_skill(
                &store,
                &store,
                &no_description,
                provenance("x"),
                DEFAULT_MAX_BUNDLE_BYTES
            )
            .await,
            Err(PublishSkillError::Bundle(SkillError::InvalidFrontMatter(_)))
        ));
        let duplicate = vec![
            file("SKILL.md", &skill_md("check-error-rates", "x")),
            file("SKILL.md", &skill_md("check-error-rates", "x")),
        ];
        assert!(matches!(
            publish_skill(
                &store,
                &store,
                &duplicate,
                provenance("x"),
                DEFAULT_MAX_BUNDLE_BYTES
            )
            .await,
            Err(PublishSkillError::Bundle(SkillError::DuplicatePath))
        ));
        assert_eq!(store.latest_skills().await?, vec![changed]);
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn skill_paths_cannot_escape_manifest() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        for path in ["../escape", "/abs/path", "a/../../b", "a\\windows"] {
            let files = vec![
                file("SKILL.md", &skill_md("check-error-rates", "x")),
                file(path, b"nope"),
            ];
            assert!(
                matches!(
                    publish_skill(
                        &store,
                        &store,
                        &files,
                        provenance("x"),
                        DEFAULT_MAX_BUNDLE_BYTES
                    )
                    .await,
                    Err(PublishSkillError::Bundle(SkillError::InvalidPath(_)))
                ),
                "{path}"
            );
        }
        assert!(store.latest_skills().await?.is_empty());
        let version = publish_skill(
            &store,
            &store,
            &bundle("v1", b"echo hi"),
            provenance("./skills/check-error-rates"),
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .await
        .unwrap();
        // Bundled scripts come back as bytes; the server never runs them.
        assert_eq!(
            read_pinned_file(&store, &store, &version.hash, "scripts/run.sh").await?,
            b"echo hi"
        );
        for path in [
            "../SKILL.md",
            "/etc/passwd",
            "scripts/../../SKILL.md",
            "missing.txt",
        ] {
            assert_eq!(
                read_pinned_file(&store, &store, &version.hash, path).await,
                Err(StoreError::NotFound),
                "{path}"
            );
        }
        // Foreign projects cannot see the version or its files.
        let foreign = f.fresh_store(scope(2)).await;
        assert_eq!(foreign.skill_version(&version.hash).await?, None);
        assert_eq!(
            read_pinned_file(&foreign, &foreign, &version.hash, "SKILL.md").await,
            Err(StoreError::NotFound)
        );
        assert!(foreign.latest_skills().await?.is_empty());
        Ok(())
    })
    .await;
}

fn rb(n: u8) -> RunbookId {
    format!("rb_{n:026}").parse().unwrap()
}
fn rbv(n: u8) -> VersionId {
    format!("rbv_{n:026}").parse().unwrap()
}
const SKILLED: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: post-deploy, title: Post-deploy}\n\
    spec:\n  skills: {error_rates: ./skills/check-error-rates}\n  \
    steps: [{id: error_rate, title: Check rates, skill: error_rates}, {id: done, title: Done}]\n";

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn runbook_skill_pins_survive_edits() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        let v1 = publish_skill(
            &store,
            &store,
            &bundle("version one", b"echo 1"),
            provenance("./skills/check-error-rates"),
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .await
        .unwrap();
        store
            .create_runbook(
                RunbookRecord {
                    runbook_id: rb(1),
                    slug: "post-deploy".into(),
                    origin: RunbookOrigin::Ui,
                    archived: false,
                },
                SKILLED.into(),
            )
            .await?;
        let context = || async {
            runbook_spec::CompileContext {
                skills: resolve_skill_pins(&store, &["./skills/check-error-rates".into()])
                    .await
                    .unwrap(),
                ..Default::default()
            }
        };
        let first = prepare("post-deploy", SKILLED, &context().await, rbv(1)).unwrap();
        assert_eq!(first.pins.skills["error_rates"], v1.hash);
        let run1 = publish_draft(&store, &store, &rb(1), 1, first).await?;

        // The library moves on; the published version keeps its pin.
        let v2 = publish_skill(
            &store,
            &store,
            &bundle("version two", b"echo 2"),
            provenance("./skills/check-error-rates"),
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .await
        .unwrap();
        assert_ne!(v2.hash, v1.hash);
        assert_eq!(
            read_pinned_file(&store, &store, &v1.hash, "SKILL.md").await?,
            skill_md("check-error-rates", "version one")
        );
        let usage = skill_usage(&store, &store, &v1.hash).await?;
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].runbook_id, rb(1));
        assert_eq!(usage[0].version_id, run1.runbook_version_id);
        assert_eq!(usage[0].step_ids, vec!["error_rate"]);

        // A new publish pins the new hash.
        store
            .save_draft(
                &rb(1),
                1,
                SKILLED.replace("Post-deploy", "Post-deploy again"),
            )
            .await?;
        let changed = SKILLED.replace("Post-deploy", "Post-deploy again");
        let second = prepare("post-deploy", &changed, &context().await, rbv(2)).unwrap();
        assert_eq!(second.pins.skills["error_rates"], v2.hash);
        let run2 = publish_draft(&store, &store, &rb(1), 2, second).await?;
        let usage2 = skill_usage(&store, &store, &v2.hash).await?;
        assert_eq!(usage2.len(), 1);
        assert_eq!(usage2[0].version_id, run2.runbook_version_id);

        // Unknown references fail resolution and compilation alike.
        assert!(
            resolve_skill_pins(&store, &["./skills/missing".into()])
                .await
                .is_err()
        );
        let diagnostics = prepare(
            "post-deploy",
            SKILLED,
            &runbook_spec::CompileContext::default(),
            rbv(9),
        )
        .unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "spec.unresolved_skill"),
            "{diagnostics:?}"
        );
        Ok(())
    })
    .await;
}

/// F39: an imported archive can hold one bundle hash under two names. A
/// lookup by hash returns the first recorded, as the memory store does,
/// instead of failing on two rows.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn one_hash_under_two_names_reads_the_first_recorded() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        let first = publish_skill(
            &store,
            &store,
            &bundle("v1", b"echo 1"),
            provenance("./skills/check-error-rates"),
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .await
        .unwrap();
        let mut renamed = first.clone();
        renamed.name = "check-error-rates-copy".into();
        store.record_skill_version(renamed).await?;
        assert_eq!(store.skill_version(&first.hash).await?, Some(first));
        assert_eq!(store.latest_skills().await?.len(), 2);
        Ok::<(), StoreError>(())
    })
    .await;
}
