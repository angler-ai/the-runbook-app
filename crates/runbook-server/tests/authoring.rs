#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use futures_util::FutureExt;
use runbook_api::{
    Request,
    authoring::{AuthoringLimits, AuthoringService},
};
use runbook_core::{
    authoring::{AuthoringFactory, AuthoringStore},
    catalog::{CatalogStore, TenantScope},
    credentials::*,
    directory::*,
    directory_operations::*,
};
use runbook_model::*;
use runbook_store_postgres::{PostgresAuthoring, PostgresCredentials, PostgresDirectory};
use serde_json::{Value, json};
use std::panic::AssertUnwindSafe;
type Api = AuthoringService<PostgresDirectory, PostgresCredentials, PostgresAuthoring>;
fn org() -> OrganizationId {
    format!("org_{:026}", 1).parse().unwrap()
}
fn project(n: u8) -> ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}
fn source(id: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {id}, title: Demo}}\nspec:\n  steps:\n    - id: work\n      title: Do work\n"
    )
}
fn token(n: u8) -> String {
    encode_secret(
        if n == 2 {
            TokenKind::Service
        } else {
            TokenKind::Personal
        },
        [n; 32],
    )
}
fn req(
    method: &str,
    path: &str,
    body: Value,
    n: u32,
    who: u8,
    key: Option<&str>,
    etag: Option<&str>,
) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        body,
        query: Default::default(),
        credential: Some(runbook_api::Credential::Bearer(token(who))),
        idempotency_key: key.map(str::to_owned),
        if_match: etag.map(str::to_owned),
        client_ip: None,
        request_id: format!("req_{n:026}"),
        new_id: format!("{n:026}"),
    }
}
async fn setup(db: &database::TestDatabase) -> Api {
    let mut store = runbook_store_postgres::PostgresStore::connect(db.config())
        .await
        .unwrap();
    store.migrate().await.unwrap();
    let d = PostgresDirectory::new(db.config().clone());
    let c = PostgresCredentials::new(db.config().clone());
    let u: UserId = format!("usr_{:026}", 1).parse().unwrap();
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org".into(),
        },
        DirectoryMutation::CreateUser(UserRecord {
            id: u.clone(),
            name: "Owner".into(),
            active: true,
            role: OrgRole::Owner,
        }),
    ];
    for n in [1, 2] {
        mutations.push(DirectoryMutation::CreateProject(ProjectRecord {
            id: project(n),
            org_id: org(),
            slug: format!("project-{n}"),
            title: "Project".into(),
        }));
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: project(n),
            user_id: u.clone(),
            role: ProjectRole::Admin,
        });
    }
    d.commit(DirectoryCommit {
        org_id: org(),
        expected_revision: 0,
        mutations,
    })
    .await
    .unwrap();
    for n in [1, 2, 3] {
        c.insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: if n == 2 {
                TokenKind::Service
            } else {
                TokenKind::Personal
            },
            hash: secret_hash(&token(n)),
            principal: if n == 2 {
                CredentialPrincipal::Service {
                    project_id: project(1),
                    role: ProjectRole::Editor,
                }
            } else {
                CredentialPrincipal::User {
                    user_id: u.clone(),
                    actor: runbook_core::credentials::ActorKind::Human,
                }
            },
            org_id: org(),
            scopes: if n == 2 {
                [Scope::RunbooksWrite, Scope::SkillsWrite].into()
            } else {
                [
                    Scope::RunbooksRead,
                    Scope::RunbooksWrite,
                    Scope::RunbooksPublish,
                    Scope::SkillsRead,
                    Scope::SkillsWrite,
                ]
                .into()
            },
            projects: if n == 3 {
                Some([project(2)].into())
            } else {
                None
            },
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    }
    AuthoringService::new(
        d,
        c,
        PostgresAuthoring::new(db.config().clone()),
        AuthoringLimits::default(),
    )
}
async fn with_api(f: impl AsyncFnOnce(Api, &database::TestDatabase)) {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let api = setup(&db).await;
        f(api, &db).await
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
async fn create(api: &Api, name: &str, n: u32, who: u8) -> runbook_api::problem::Response {
    api.checked(req(
        "POST",
        &format!("/v1/projects/{}/runbooks", project(1)),
        json!({"source":source(name)}),
        n,
        who,
        Some(name),
        None,
    ))
    .await
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn write_only_agent_cannot_publish() {
    with_api(async |api, _| {
        let a = create(&api, "demo", 10, 2).await;
        assert_eq!(a.status, 201, "{:?}", a.body);
        let rb = a.body["runbook_id"].as_str().unwrap();
        let draft = format!("/v1/runbooks/{rb}/draft");
        let versions = format!("/v1/runbooks/{rb}/versions");
        let b = api
            .checked(req(
                "PATCH",
                &draft,
                json!({"source":source("demo")+"# second\n"}),
                11,
                2,
                None,
                a.etag.as_deref(),
            ))
            .await;
        assert_eq!(b.status, 200);
        let denied = api
            .checked(req(
                "POST",
                &versions,
                json!({}),
                12,
                2,
                Some("publish"),
                b.etag.as_deref(),
            ))
            .await;
        assert_eq!(denied.status, 403);
        assert_eq!(denied.body["code"], "auth.scope_missing");
        assert_eq!(
            api.checked(req("GET", &draft, Value::Null, 13, 2, None, None))
                .await
                .status,
            403
        );
        assert_eq!(
            api.checked(req("GET", &draft, Value::Null, 14, 3, None, None))
                .await
                .status,
            403
        );
        let published = api
            .checked(req(
                "POST",
                &versions,
                json!({}),
                15,
                1,
                Some("publish"),
                b.etag.as_deref(),
            ))
            .await;
        assert_eq!(published.status, 201, "{:?}", published.body);
        assert_eq!(published.body["runbook_version"], 1);
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn draft_headers_select_exact_revision() {
    with_api(async |api, _| {
        let a = create(&api, "demo", 10, 1).await;
        let rb = a.body["runbook_id"].as_str().unwrap();
        let path = format!("/v1/runbooks/{rb}/draft");
        for (tag, status) in [(None, 428), (Some("*"), 412), (Some("\"bad\""), 412)] {
            assert_eq!(
                api.checked(req(
                    "PATCH",
                    &path,
                    json!({"source":"invalid ["}),
                    11,
                    1,
                    None,
                    tag
                ))
                .await
                .status,
                status
            );
        }
        let b = api
            .checked(req(
                "PATCH",
                &path,
                json!({"source":"invalid ["}),
                11,
                1,
                None,
                a.etag.as_deref(),
            ))
            .await;
        assert_eq!(b.status, 200);
        assert!(!b.body["diagnostics"].as_array().unwrap().is_empty());
        assert_ne!(b.etag, a.etag);
        let mut limited = api.clone();
        limited.limits.source_bytes = 10;
        assert_eq!(
            limited
                .checked(req(
                    "PATCH",
                    &path,
                    json!({"source":"x".repeat(11)}),
                    12,
                    1,
                    None,
                    b.etag.as_deref()
                ))
                .await
                .body["code"],
            "spec.source_limit"
        );
        let path = format!("/v1/projects/{}/runbooks", project(1));
        for (body, code, n) in [
            (json!({"source":"[","slug":"explicit"}), "", 20),
            (json!({"source":source("inferred")}), "", 21),
            (json!({"source":"["}), "runbook.slug_required", 22),
            (
                json!({"source":source("inferred"),"slug":"other"}),
                "runbook.id_mismatch",
                23,
            ),
        ] {
            let r = api
                .checked(req("POST", &path, body, n, 1, Some(&n.to_string()), None))
                .await;
            if code.is_empty() {
                assert_eq!(r.status, 201, "{:?}", r.body);
            } else {
                assert_eq!(r.status, 422);
                assert_eq!(r.body["code"], code);
            }
        }
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn validation_does_not_save() {
    with_api(async |api, _| {
        let r = api
            .checked(req(
                "POST",
                "/v1/validate",
                json!({"source":source("demo")}),
                10,
                1,
                None,
                None,
            ))
            .await;
        assert_eq!(r.status, 200);
        assert_eq!(r.body["step_budget"], 1, "{:?}", r.body);
        let r = api
            .checked(req(
                "POST",
                "/v1/validate",
                json!({"source":"["}),
                11,
                1,
                None,
                None,
            ))
            .await;
        assert_eq!(r.status, 200);
        assert!(!r.body["diagnostics"].as_array().unwrap().is_empty());
        let r = api
            .checked(req(
                "GET",
                &format!("/v1/projects/{}/runbooks", project(1)),
                Value::Null,
                12,
                1,
                None,
                None,
            ))
            .await;
        assert_eq!(r.body["items"], json!([]));
        assert_eq!(
            api.checked(req(
                "POST",
                "/v1/validate",
                json!({"source":source("demo"),"project":project(1)}),
                13,
                3,
                None,
                None
            ))
            .await
            .status,
            403
        );
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn post_retry_does_not_duplicate_authoring() {
    with_api(async |api, db| {
        let (a, b) = tokio::join!(create(&api, "demo", 10, 1), create(&api, "demo", 11, 1));
        assert_eq!(a.status, 201);
        assert_eq!(a.body, b.body);
        let rb = a.body["runbook_id"].as_str().unwrap();
        let versions = format!("/v1/runbooks/{rb}/versions");
        let (one, two) = tokio::join!(
            api.checked(req(
                "POST",
                &versions,
                json!({}),
                12,
                1,
                Some("publish"),
                a.etag.as_deref()
            )),
            api.checked(req(
                "POST",
                &versions,
                json!({}),
                13,
                1,
                Some("publish"),
                a.etag.as_deref()
            ))
        );
        assert_eq!(one.status, 201, "{:?}", one.body);
        assert_eq!(one.body, two.body);
        let edit = api
            .checked(req(
                "PATCH",
                &format!("/v1/runbooks/{rb}/draft"),
                json!({"source":source("demo")+"# changed\n"}),
                14,
                1,
                None,
                a.etag.as_deref(),
            ))
            .await;
        assert_eq!(edit.status, 200);
        let restarted = AuthoringService::new(
            PostgresDirectory::new(db.config().clone()),
            PostgresCredentials::new(db.config().clone()),
            PostgresAuthoring::new(db.config().clone()),
            AuthoringLimits::default(),
        );
        let replay = restarted
            .checked(req(
                "POST",
                &versions,
                json!({}),
                15,
                1,
                Some("publish"),
                a.etag.as_deref(),
            ))
            .await;
        assert_eq!(replay.body, one.body);
        let changed = api
            .checked(req(
                "POST",
                &versions,
                json!({}),
                16,
                1,
                Some("publish"),
                edit.etag.as_deref(),
            ))
            .await;
        assert_eq!(changed.status, 409);
    })
    .await;
}

trait Checked {
    async fn checked(&self, r: Request) -> runbook_api::problem::Response;
}
impl Checked for Api {
    async fn checked(&self, r: Request) -> runbook_api::problem::Response {
        let route = AUTHORING_ROUTES.iter().find(|route| {
            route.method == r.method
                && route.path.split('/').count() == r.path.split('/').count()
                && route
                    .path
                    .split('/')
                    .zip(r.path.split('/'))
                    .all(|(a, b)| a.starts_with('{') || a == b)
        });
        let response = self.handle(r).await;
        let schema = if response.problem {
            "Problem"
        } else {
            route.unwrap().response.unwrap()
        };
        let doc = runbook_contract::openapi();
        let root =
            json!({"$ref":format!("#/components/schemas/{schema}"),"components":doc["components"]});
        if let Err(e) = jsonschema::validator_for(&root)
            .unwrap()
            .validate(&response.body)
        {
            panic!("{schema}: {e}");
        }
        response
    }
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn project_context_and_skill_versions_are_pinned() {
    with_api(async |api,_|{
 use base64::Engine;
 let skill_path=format!("/v1/projects/{}/skills",project(1));
 let bundle=|body:&str|json!({"files":[{"path":"SKILL.md","content":base64::engine::general_purpose::STANDARD.encode(format!("---\nname: inspect\ndescription: Inspect a service.\n---\n{body}"))}],"reference":"./skills/inspect"});
 let skill=api.checked(req("POST",&skill_path,bundle("first"),10,1,Some("skill"),None)).await;assert_eq!(skill.status,201,"{:?}",skill.body);
 assert_eq!(skill.body["latest"],json!(true),"the only version of a name is its latest");
 let same=api.checked(req("POST",&skill_path,bundle("first"),11,1,Some("skill"),None)).await;assert_eq!(same.body,skill.body);
 assert_eq!(api.checked(req("POST",&skill_path,json!({"files":[]}),12,1,Some("skill"),None)).await.status,409);
 let mut bad=bundle("bad");bad["files"][0]["path"]=json!("../escape");assert_eq!(api.checked(req("POST",&skill_path,bad,12,1,Some("bad"),None)).await.status,422);
 let before=api.directory.snapshot(&org()).await.unwrap();
 api.directory.commit(DirectoryCommit{org_id:org(),expected_revision:before.revision,mutations:vec![DirectoryMutation::CreateTeam(TeamRecord{id:format!("team_{:026}",1).parse().unwrap(),org_id:org(),slug:"oncall".into(),title:"On call".into()})]}).await.unwrap();
 let before=api.directory.snapshot(&org()).await.unwrap();
 api.directory.apply_operation(DirectoryOperation{commit:DirectoryCommit{org_id:org(),expected_revision:before.revision,mutations:vec![]},variables:Some(VariablesWrite{project_id:project(1),expected_revision:0,variables:serde_json::from_value(json!({"declarations":{"region":{"type":"string"}},"values":{"region":"us"}})).unwrap()}),replay:None,response:StoredResponse{status:200,body:Value::Null,etag:None}}).await.unwrap();
 let contextual="apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: contextual, title: Context, owners: ['team:oncall']}\nspec:\n  vars: {region: '${{ project.vars.region }}'}\n  steps: [{id: work, title: Work, skill: 'skill://inspect'}]\n";
 let aliased=contextual.replace("  steps:","  skills: {inspect: './another/folder/inspect'}\n  steps:").replace("skill: 'skill://inspect'","skill: inspect");
 let validated_alias=api.checked(req("POST","/v1/validate",json!({"source":aliased,"project":project(1)}),19,1,None,None)).await;assert_eq!(validated_alias.body["diagnostics"],json!([]),"{:?}",validated_alias.body);
 for p in [None,Some(json!(project(2))),Some(json!("project-1"))] {
  let mut body=json!({"source":contextual});if let Some(p)=p.clone(){body["project"]=p;}
  let r=api.checked(req("POST","/v1/validate",body,20,1,None,None)).await;assert_eq!(r.status,200);
  if p==Some(json!("project-1")){assert_eq!(r.body["diagnostics"],json!([]),"{:?}",r.body);}else{assert!(!r.body["diagnostics"].as_array().unwrap().is_empty());}
 }
 let draft=api.checked(req("POST",&format!("/v1/projects/{}/runbooks",project(1)),json!({"source":contextual}),21,1,Some("contextual"),None)).await;assert_eq!(draft.status,201);
 let rb=draft.body["runbook_id"].as_str().unwrap();
 let published=api.checked(req("POST",&format!("/v1/runbooks/{rb}/versions"),json!({}),22,1,Some("publish"),draft.etag.as_deref())).await;assert_eq!(published.status,201,"{:?}",published.body);
 let newer=api.checked(req("POST",&skill_path,bundle("second"),23,1,Some("skill-new"),None)).await;assert_ne!(newer.body["hash"],skill.body["hash"]);
 // `latest` is compile_context's notion: per name, the most recently recorded version.
 assert_eq!(newer.body["latest"],json!(true));
 let again=api.checked(req("POST",&skill_path,bundle("first"),23,1,Some("skill-again"),None)).await;assert_eq!(again.body["hash"],skill.body["hash"]);
 assert_eq!(again.body["latest"],json!(false),"re-uploading an older version does not make it latest");
 let listed=api.checked(req("GET",&skill_path,Value::Null,23,1,None,None)).await;
 let flags:Vec<(Value,Value)>=listed.body["items"].as_array().unwrap().iter().map(|i|(i["hash"].clone(),i["latest"].clone())).collect();
 assert_eq!(flags.len(),2,"{flags:?}");assert!(flags.contains(&(newer.body["hash"].clone(),json!(true))),"{flags:?}");assert!(flags.contains(&(skill.body["hash"].clone(),json!(false))),"{flags:?}");
 let latest_get=api.checked(req("GET",&format!("{skill_path}/{}",newer.body["hash"].as_str().unwrap()),Value::Null,23,1,None,None)).await;assert_eq!(latest_get.body["latest"],json!(true));
 let old=api.checked(req("GET",&format!("{skill_path}/{}",skill.body["hash"].as_str().unwrap()),Value::Null,24,1,None,None)).await;assert_eq!(old.body["usages"][0]["runbook_id"],rb);assert_eq!(old.body["latest"],json!(false));assert_eq!(old.body["usages"][0]["step_ids"],json!(["work"]));
 let store=api.factory.scoped(TenantScope{org_id:org(),project_id:project(1)});let v=store.version_data(&rb.parse().unwrap(),None).await.unwrap();assert_eq!(runbook_core::authoring::hex(&v.pins.skills["skill://inspect"]),skill.body["hash"]);
 let mut foreign=req("GET",&skill_path,Value::Null,25,3,None,None);assert_eq!(api.checked(foreign).await.status,403);
 foreign=req("GET",&format!("/v1/projects/{}/skills",project(2)),Value::Null,26,3,None,None);assert_eq!(api.checked(foreign).await.body["items"],json!([]));
 let mut list=req("GET",&skill_path,Value::Null,27,1,None,None);list.query.insert("limit".into(),"1".into());let first=api.checked(list).await;assert_eq!(first.body["items"].as_array().unwrap().len(),1);
 let mut wrong=req("GET",&format!("/v1/projects/{}/skills",project(2)),Value::Null,28,1,None,None);wrong.query.insert("cursor".into(),first.body["next_cursor"].as_str().unwrap().into());assert_eq!(api.checked(wrong).await.status,400);
}).await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn authoring_failures_roll_back_and_reads_preserve_versions() {
    with_api(async |api, _| {
        api.factory.fail_before_replay();
        assert_eq!(create(&api, "demo", 10, 1).await.status, 503);
        let path = format!("/v1/projects/{}/runbooks", project(1));
        assert_eq!(
            api.checked(req("GET", &path, Value::Null, 11, 1, None, None))
                .await
                .body["items"],
            json!([])
        );
        let draft = create(&api, "demo", 12, 1).await;
        assert_eq!(draft.status, 201);
        let rb = draft.body["runbook_id"].as_str().unwrap();
        let versions = format!("/v1/runbooks/{rb}/versions");
        for (tag, status) in [(None, 428), (Some("*"), 412), (Some("W/\"bad\""), 412)] {
            assert_eq!(
                api.checked(req(
                    "POST",
                    &versions,
                    json!({}),
                    13,
                    1,
                    Some("missing-precondition"),
                    tag
                ))
                .await
                .status,
                status
            );
        }
        api.factory.fail_before_replay();
        let failed = api
            .checked(req(
                "POST",
                &versions,
                json!({}),
                14,
                1,
                Some("publish"),
                draft.etag.as_deref(),
            ))
            .await;
        assert_eq!(failed.status, 503);
        assert_eq!(
            api.checked(req("GET", &versions, Value::Null, 15, 1, None, None))
                .await
                .body["items"],
            json!([])
        );
        let first = api
            .checked(req(
                "POST",
                &versions,
                json!({}),
                16,
                1,
                Some("publish"),
                draft.etag.as_deref(),
            ))
            .await;
        assert_eq!(first.body["runbook_version"], 1);
        let updated_source = source("demo").replace("id: work", "id: next");
        let edit = api
            .checked(req(
                "PATCH",
                &format!("/v1/runbooks/{rb}/draft"),
                json!({"source":updated_source}),
                17,
                1,
                None,
                draft.etag.as_deref(),
            ))
            .await;
        let second = api
            .checked(req(
                "POST",
                &versions,
                json!({}),
                18,
                1,
                Some("publish-next"),
                edit.etag.as_deref(),
            ))
            .await;
        assert_eq!(second.body["runbook_version"], 2);
        let old = api
            .checked(req(
                "GET",
                &format!("{versions}/1"),
                Value::Null,
                19,
                1,
                None,
                None,
            ))
            .await;
        assert_eq!(old.body["source"], source("demo"));
        let diff = api
            .checked(req(
                "GET",
                &format!("{versions}/1/diff/2"),
                Value::Null,
                20,
                1,
                None,
                None,
            ))
            .await;
        assert_eq!(diff.body["added_steps"], json!(["next"]));
        assert_eq!(diff.body["removed_steps"], json!(["work"]));
        assert!(
            diff.body["unified_diff"]
                .as_str()
                .unwrap()
                .contains("-    - id: work")
        );
        let store = api.factory.scoped(TenantScope {
            org_id: org(),
            project_id: project(1),
        });
        let catalog = store.catalog();
        let prepared = runbook_core::publish::prepare(
            "demo",
            &source("demo"),
            &Default::default(),
            format!("rbv_{:026}", 30).parse().unwrap(),
        )
        .unwrap();
        runbook_core::publish::publish_preview(
            catalog,
            catalog,
            &rb.parse().unwrap(),
            prepared,
            Some("feature".into()),
            Some("commit".into()),
        )
        .await
        .unwrap();
        let mut preview = req("GET", &versions, Value::Null, 31, 1, None, None);
        preview.query.insert("preview".into(), "true".into());
        preview.query.insert("branch".into(), "feature".into());
        let rows = api.checked(preview).await;
        assert_eq!(rows.body["items"].as_array().unwrap().len(), 1);
        assert!(rows.body["items"][0]["runbook_version"].is_null());
        let latest = api
            .checked(req(
                "GET",
                &format!("{versions}/latest"),
                Value::Null,
                32,
                1,
                None,
                None,
            ))
            .await;
        assert_eq!(latest.body["runbook_version"], 2);
        let git: RunbookId = format!("rb_{:026}", 40).parse().unwrap();
        catalog
            .create_runbook(
                runbook_core::catalog::RunbookRecord {
                    runbook_id: git.clone(),
                    slug: "git".into(),
                    origin: runbook_core::catalog::RunbookOrigin::Git,
                    archived: false,
                },
                source("git"),
            )
            .await
            .unwrap();
        for (method, suffix, body) in [
            ("PATCH", "draft", json!({"source":"invalid"})),
            ("POST", "versions", json!({})),
        ] {
            assert_eq!(
                api.checked(req(
                    method,
                    &format!("/v1/runbooks/{git}/{suffix}"),
                    body,
                    41,
                    1,
                    Some("git"),
                    Some("\"anything\"")
                ))
                .await
                .body["code"],
                "runbook.git_managed"
            );
        }
        api.credentials
            .revoke(&org(), &format!("tok_{:026}", 1).parse().unwrap(), 1)
            .await
            .unwrap();
        assert_eq!(
            api.checked(req(
                "POST",
                &versions,
                json!({}),
                42,
                1,
                Some("publish"),
                draft.etag.as_deref()
            ))
            .await
            .status,
            401
        );
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn authoring_routes_cross_native_transport() {
    with_api(async |_, db| {
        use axum::{
            body::{Body, to_bytes},
            http::Request as HttpRequest,
        };
        use tower::ServiceExt;
        let mut config = runbook_server::config::ServerConfig::new(
            db.config().clone(),
            "https://instance.example",
            "127.0.0.1:0",
        )
        .unwrap();
        config.authoring.source_bytes = 1024;
        config.authoring.skill_bytes = 4096;
        let worker = runbook_server::http::Worker::start(config).unwrap();
        let router = runbook_server::http::router(worker.client());
        let r = router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(format!("/v1/projects/{}/runbooks", project(1)))
                    .header("authorization", format!("Bearer {}", token(1)))
                    .header("content-type", "application/json")
                    .header("idempotency-key", "native")
                    .body(Body::from(json!({"source":source("native")}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        assert!(r.headers().contains_key("etag"));
        assert!(r.headers().contains_key("x-request-id"));
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 1_000_000).await.unwrap()).unwrap();
        assert_eq!(body["revision"], 1);
        let r = router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/v1/meta")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let meta: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 1_000_000).await.unwrap()).unwrap();
        assert_eq!(meta["capabilities"]["authoring"], true);
        assert_eq!(meta["limits"]["source_bytes"], 1024);
        assert_eq!(meta["limits"]["skill_bundle_bytes"], 4096);
        let r = router
            .oneshot(
                HttpRequest::builder()
                    .method("DELETE")
                    .uri("/v1/validate")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 405);
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn authoring_key_reads_are_org_scoped() {
    with_api(async |api, _| {
        let other_org: OrganizationId = format!("org_{:026}", 2).parse().unwrap();
        let other_project = project(3);
        let other_user: UserId = format!("usr_{:026}", 2).parse().unwrap();
        api.directory
            .commit(DirectoryCommit {
                org_id: other_org.clone(),
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Other".into(),
                    },
                    DirectoryMutation::CreateUser(UserRecord {
                        id: other_user,
                        name: "Other".into(),
                        role: OrgRole::Owner,
                        active: true,
                    }),
                    DirectoryMutation::CreateProject(ProjectRecord {
                        id: other_project.clone(),
                        org_id: other_org.clone(),
                        slug: "other".into(),
                        title: "Other".into(),
                    }),
                ],
            })
            .await
            .unwrap();
        let foreign: RunbookId = format!("rb_{:026}", 90).parse().unwrap();
        let other = api.factory.scoped(TenantScope {
            org_id: other_org,
            project_id: other_project,
        });
        other
            .catalog()
            .create_runbook(
                runbook_core::catalog::RunbookRecord {
                    runbook_id: foreign.clone(),
                    slug: "foreign".into(),
                    origin: runbook_core::catalog::RunbookOrigin::Ui,
                    archived: false,
                },
                source("foreign"),
            )
            .await
            .unwrap();
        for path in [
            format!("/v1/runbooks/{foreign}/draft"),
            format!("/v1/runbooks/{foreign}/versions/1"),
            format!("/v1/runbooks/{foreign}"),
        ] {
            assert_eq!(
                api.checked(req("GET", &path, Value::Null, 91, 1, None, None))
                    .await
                    .status,
                404
            );
        }
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn arbitrary_utf8_drafts_round_trip_nul() {
    with_api(async |api, _| {
        let path = format!("/v1/projects/{}/runbooks", project(1));
        let first = "before\0after";
        let created = api
            .checked(req(
                "POST",
                &path,
                json!({"slug":"nul-draft","source":first}),
                10,
                1,
                Some("nul-create"),
                None,
            ))
            .await;
        assert_eq!(created.status, 201, "{:?}", created.body);
        assert_eq!(created.body["source"], first);
        assert!(!created.body["diagnostics"].as_array().unwrap().is_empty());
        let draft = format!(
            "/v1/runbooks/{}/draft",
            created.body["runbook_id"].as_str().unwrap()
        );
        let saved = api
            .checked(req(
                "PATCH",
                &draft,
                json!({"source":"\0"}),
                11,
                1,
                None,
                created.etag.as_deref(),
            ))
            .await;
        assert_eq!(saved.status, 200);
        assert_eq!(saved.body["source"], "\0");
        assert_eq!(
            api.checked(req("GET", &draft, Value::Null, 12, 1, None, None))
                .await
                .body["source"],
            "\0"
        );
        let replay = api
            .checked(req(
                "POST",
                &path,
                json!({"slug":"nul-draft","source":first}),
                13,
                1,
                Some("nul-create"),
                None,
            ))
            .await;
        assert_eq!(replay.body, created.body);
    })
    .await;
}
