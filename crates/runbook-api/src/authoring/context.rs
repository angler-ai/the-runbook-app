use super::*;
use yaml_rust2::YamlLoader;
pub(super) fn creation_slug(r: &Request, b: &CreateRunbookRequest) -> Result<String, Error> {
    let parsed = bounded_yaml(&b.source);
    let embedded = parsed
        .as_ref()
        .filter(|docs| docs.len() == 1)
        .and_then(|docs| docs[0]["metadata"]["id"].as_str());
    if let (Some(explicit), Some(embedded)) = (&b.slug, embedded)
        && explicit != embedded
    {
        return Err(error(r, 422, "runbook.id_mismatch"));
    }
    let slug = b
        .slug
        .as_deref()
        .or(embedded)
        .ok_or_else(|| error(r, 422, "runbook.slug_required"))?;
    if parse_runbook_ref(slug).is_err() || slug.contains('@') {
        return Err(error(r, 422, "catalog.invalid_slug"));
    }
    Ok(slug.into())
}
/// The `/v1/validate` body. The browser compiler (`runbook-web-wasm`) calls the
/// same `runbook_spec::authoring` functions, and its parity test pins it here.
pub fn validate(source: &str, context: &runbook_spec::CompileContext) -> ValidateResponse {
    runbook_spec::authoring::validate_source(source, context)
}
impl<D: DirectoryOperations, C: CredentialStore, F: AuthoringFactory> AuthoringService<D, C, F> {
    /// Read the directory facts, then build the context with the pure
    /// `runbook_spec::authoring::compile_context` the browser compiler also uses.
    pub(super) async fn compile_context(
        &self,
        r: &Request,
        s: &DirectorySnapshot,
        project: &ProjectId,
        store: &F::Store,
        source: &str,
    ) -> Result<runbook_spec::CompileContext, Error> {
        use runbook_spec::authoring::{
            CompileDirectory, DirectoryTeam, DirectoryUser, LatestSkill, compile_context,
        };
        let variables = self
            .directory
            .project_variables(&s.org_id, project)
            .await
            .map_err(|_| error(r, 503, "storage.unavailable"))?;
        let latest = store
            .catalog()
            .latest_skills()
            .await
            .map_err(|e| store_error(r, e))?;
        let directory = CompileDirectory {
            users: s
                .users
                .values()
                .map(|u| DirectoryUser {
                    id: u.id.clone(),
                    name: u.name.clone(),
                    active: u.active,
                })
                .collect(),
            teams: s
                .teams
                .values()
                .map(|t| DirectoryTeam {
                    id: t.id.clone(),
                    slug: t.slug.clone(),
                })
                .collect(),
            skills: latest
                .into_iter()
                .map(|v| LatestSkill {
                    name: v.name,
                    reference: v.provenance.reference,
                    hash: v.hash,
                })
                .collect(),
            variables: variables.variables.declarations.into_iter().collect(),
        };
        let mut context = compile_context(&directory, source)
            .map_err(|_| error(r, 503, "vars.invalid_stored_declarations"))?;
        // LANG4: the project's runbooks this source calls, pinned from the
        // catalog (a reference that does not resolve stays out, and the
        // compiler reports it).
        if source.contains("call") {
            let runbooks = store
                .catalog_runbooks()
                .await
                .map_err(|e| store_error(r, e))?;
            context.runbooks =
                runbook_core::calls::call_targets(store.catalog(), &runbooks, source)
                    .await
                    .map_err(|e| store_error(r, e))?;
        }
        Ok(context)
    }
}

// Lenient shape extraction, but never unbounded alias expansion or nesting.
fn bounded_yaml(source: &str) -> Option<Vec<yaml_rust2::Yaml>> {
    use yaml_rust2::parser::{Event, Parser};
    let mut parser = Parser::new_from_str(source);
    let mut depth = 0usize;
    let mut count = 0usize;
    loop {
        let (event, _) = parser.next_token().ok()?;
        count += 1;
        if count > 262_144 {
            return None;
        }
        match event {
            Event::Alias(_)
            | Event::Scalar(_, _, 1.., _)
            | Event::Scalar(_, _, _, Some(_))
            | Event::MappingStart(1.., _)
            | Event::SequenceStart(1.., _)
            | Event::MappingStart(_, Some(_))
            | Event::SequenceStart(_, Some(_)) => return None,
            Event::MappingStart(_, _) | Event::SequenceStart(_, _) => {
                depth += 1;
                if depth > 64 {
                    return None;
                }
            }
            Event::MappingEnd | Event::SequenceEnd => depth = depth.checked_sub(1)?,
            Event::StreamEnd => break,
            _ => {}
        }
    }
    YamlLoader::load_from_str(source).ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn slug(source: &str, slug: Option<&str>) -> Result<String, String> {
        let r = Request {
            method: "POST".into(),
            path: "/v1/projects/p/runbooks".into(),
            body: Value::Null,
            query: Default::default(),
            credential: None,
            idempotency_key: None,
            request_id: "test".into(),
            new_id: "unused".into(),
            if_match: None,
            client_ip: None,
        };
        creation_slug(
            &r,
            &CreateRunbookRequest {
                source: source.into(),
                slug: slug.map(str::to_owned),
            },
        )
        .map_err(|r| r.body["code"].as_str().unwrap().to_owned())
    }
    #[test]
    fn creation_slug_only_accepts_invalid_draft() {
        assert_eq!(slug("[", Some("demo")), Ok("demo".into()));
    }
    #[test]
    fn creation_metadata_id_only_is_lenient() {
        assert_eq!(slug("metadata: {id: demo}", None), Ok("demo".into()));
    }
    #[test]
    fn creation_without_slug_or_metadata_is_rejected() {
        assert_eq!(slug("[", None), Err("runbook.slug_required".into()));
    }
    #[test]
    fn creation_explicit_and_embedded_slug_must_match() {
        assert_eq!(
            slug("metadata: {id: demo}", Some("other")),
            Err("runbook.id_mismatch".into())
        );
    }
    #[test]
    fn creation_rejects_invalid_slug_and_bounds_yaml_expansion() {
        assert_eq!(
            slug("[", Some("Not A Slug")),
            Err("catalog.invalid_slug".into())
        );
        assert!(bounded_yaml("x: &x [1,2]\ny: [*x,*x]").is_none());
        assert!(bounded_yaml(&format!("x: {}0{}", "[".repeat(65), "]".repeat(65))).is_none());
    }
}
