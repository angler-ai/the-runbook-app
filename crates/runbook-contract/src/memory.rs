//! MEM3: the six runbook memory operations (PRD §7, §9; D40, D41), and MEM6's
//! revision history. MEM6 (user decision 2026-11-13): agents write live
//! memory directly; people keep review, archive and revert.
use serde_json::{Value, json};

/// Describe one memory operation in place: authorization alternatives, the
/// query each GET takes, the `If-Match` fence on PATCH and the error codes.
pub(crate) fn describe(path: &str, method: &str, operation: &mut Value) {
    let problem = super::problem_response;
    let scope = if method == "get" {
        "runbooks:read"
    } else {
        "runbooks:write"
    };
    let history = path == "/v1/memory/{mem}/revisions";
    let mut alternatives = vec![json!({"credential":"scoped_principal","scopes":[scope]})];
    if !history {
        let mut execution = json!({"credential":"execution_access","resource":"own_runbook_or_own_project",
            "status":if method == "post" { "active_or_proposed" } else { "active" }});
        if method == "post" {
            execution["source"] = json!("own_current_step_run");
        }
        if method == "patch" {
            execution["actions"] = json!(["edit"]);
        }
        alternatives.push(execution);
    }
    operation["x-implementation"] = json!("implemented");
    operation["x-authorization-alternatives"] = json!(alternatives);
    if history {
        operation["summary"] = json!("List a memory entry's revisions and who made each");
        operation["description"] = json!(
            "MEM6. Requires runbooks:read on a person's own credential (session or personal token). The entry's kept revisions, oldest first, a page at a time (limit 1-100, default 50; pass next_cursor as cursor). Each has its action (create, propose, edit, accept, reject, archive, drop, lock, unlock, restore), the writer's kind (person, agent, service, execution, system), the writer's non-secret label (a person's user ID; an agent's token and sponsor; a service token; an execution and the step run it claimed) and the entry as it stood after it. An entry keeps its first revision, the newest revision whose content a person wrote, and its newest 50; pruned counts the revisions removed between them. An agent's or service's token gets 404, as for ?revision= reads; execution credentials get 403. To revert, PATCH {action: restore, revision: n}. An entry in another project of the organization is 404."
        );
        operation["responses"]["403"] = problem(
            "Credential lacks the required scope or resource access.",
            &["auth.scope_missing", "auth.forbidden"],
        );
        operation["parameters"].as_array_mut().unwrap().extend([
            json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":50}}),
            json!({"name":"cursor","in":"query","description":"next_cursor of the previous page.","schema":{"type":"string"}}),
        ]);
        return;
    }
    let entry = path == "/v1/memory/{mem}";
    operation["summary"] = json!(match (method, entry) {
        ("get", false) => "Search memory entries",
        ("post", _) => "Save a live memory entry, or propose one for review",
        ("get", true) => "Read a memory entry or one of its revisions",
        _ => "Edit a memory entry; people also accept, reject or archive",
    });
    operation["description"] = json!(match (method, entry) {
        ("get", false) =>
            "Implemented on Postgres (MEM3). Requires runbooks:read OR an execution access credential. A runbook's memory is its entries plus the project's; a project's memory is every entry in the project. status defaults to active; only a person's credential may ask for proposed or archived entries or include_expired (an agent's or service's token is always given active, unexpired entries). A runbook or project in another project of the organization is 404. Newest first (update time, then ID); the cursor is bound to scope and filters. q matches when every word appears in the title or body (case-insensitive, no stemming); tags is comma-separated and every tag must match; expired entries are left out unless include_expired=true. Execution credentials read only active, unexpired entries of their own runbook and project (status other than active or include_expired is 403). Memory is labeled untrusted data and never changes instructions or control flow.",
        ("post", _) =>
            "Implemented on Postgres and Cloudflare (MEM3, MEM6). Agents write live memory directly (MEM6, user decision 2026-11-13): runbooks:write on any principal credential creates status: active, with a source naming the writer: person {user_id} for a person's own credential (session or personal token), agent {token_id, sponsor_user_id} for an agent-kind personal token, service {token_id} for a service token; last_writer names whoever made the current revision. propose: true saves status: proposed instead, a proposal a person accepts or rejects. An agent's or service's save outside a run emits memory.saved (or memory.proposed), identifiers only. A retry with the same Idempotency-Key answers the entry as first created (revision 1), or 404 to an agent or service once the entry is archived or expired. An execution access credential saves from the step run it currently claims (D40), and only if the credential that issued it holds runbooks:write (else 403 auth.scope_missing): the server derives execution ID, step run ID and actor from the claim and records MemorySaved (active; emits memory.saved) or, with propose: true, MemoryProposed (emits memory.proposed) in the execution, listed on the step's memory_saved or memory_proposed line of the execution document. Runbook scope must be its own runbook; project scope must be its own project. step_id is a relevance filter, not source identity or authorization: a step path (step IDs joined by '.', with #n loop and [key] foreach iterations) naming a step of the runbook's draft or latest published version (422 memory.invalid_step_id); an execution credential's must be the step it claims or its step key (409 memory.step_mismatch). Titles, tags and step IDs are printable text without control characters (422 memory.invalid_title, memory.invalid_tag, memory.invalid_step_id). Titles, bodies, tags and step IDs pass log-equivalent secret redaction before storage. Limits: 500 active entries and 100 pending proposals per runbook (or per project for project entries), counted separately, of which agents, services and runs may fill 450 active entries (the last 50 are people's); 20 memory writes (saves, proposals and edits) per execution; a 4 KB body. On Cloudflare a run's write that races a full quota after its run committed is kept as an archived marker (dropped: memory.limit_reached), listed on the step's memory_saved or memory_proposed line of the execution document; archives leave markers out.",
        ("get", true) =>
            "Implemented on Postgres and Cloudflare (MEM3). Requires runbooks:read; the ETag names the current revision. revision=n reads that prior revision (edits never rewrite history); GET /v1/memory/{mem}/revisions lists them with their writers. An agent's or service's token reads only the current revision of an active, unexpired entry (anything else is 404); execution credentials read only an active, unexpired entry of their own runbook or project, never a past revision. An entry in another project of the organization is 404.",
        _ =>
            "Implemented on Postgres and Cloudflare (MEM3, MEM6). Requires runbooks:write. If-Match must repeat the entry's ETag. A person edits, accepts, rejects, archives, locks, unlocks or restores: accept activates a proposal if an active slot is free (409 memory.limit_reached); reject and archive set archived and keep history; edit applies to proposed or active entries; lock stops agents editing the entry until unlock; restore {revision} makes that revision's content current again as a new revision, unarchiving the entry and skipping the step check (404 memory.revision_not_found for a pruned or unknown revision; a drop marker is never restored, 409 memory.invalid_transition). An agent-kind or service token, and an execution access credential for its own runbook or project, only edit, and only an active, unexpired, unlocked entry. For them an entry they cannot see is 404 (403 for an execution credential) before any other answer; accept, reject, archive, lock, unlock and restore are 403 memory.person_required; a locked entry is 403 memory.locked; an expiry already set stays (they may set or shorten one, never clear or extend it), and an expiry less than a minute away is an archive (403 memory.person_required). Agents, services and runs together edit one entry at most 20 times per UTC day (409 memory.edit_limit). The lock and that limit hold inside the store's revision fence. An execution credential must hold a live claim (409 memory.step_not_claimed), its issuer must hold runbooks:write (403 auth.scope_missing), and the edit counts toward the run's 20 memory writes (409 memory.proposal_limit); the run records MemoryEdited, lists it on the step's memory_edited line and emits memory.edited. An agent's or service's edit outside a run emits memory.edited, identifiers only. Each revision records its writer (GET /v1/memory/{mem}/revisions); an entry keeps its first revision, its newest person-written revision and its newest 50. last_writer changes only with content (create, edit, restore, accept), never with lock or unlock. Writes pass log-equivalent secret redaction.",
    });
    operation["responses"]["403"] = problem(
        "Credential lacks the required scope or resource access.",
        &[
            "auth.scope_missing",
            "auth.forbidden",
            "memory.person_required",
            "memory.locked",
        ],
    );
    let entry_headers = json!({"ETag":{"description":"Strong ETag of the entry's current revision; repeat it verbatim in If-Match.","schema":{"type":"string"}}});
    if method == "get" && entry {
        operation["responses"]["200"]["headers"] = entry_headers.clone();
    }
    if method == "post" {
        operation["responses"]["201"]["headers"] = entry_headers.clone();
        operation["responses"]["409"] = problem(
            "A limit is reached or the proposal has no claimed step run; nothing is saved.",
            &[
                "memory.limit_reached",
                "memory.proposal_limit",
                "memory.step_not_claimed",
                "memory.step_mismatch",
                "memory.step_run_ambiguous",
                "request.idempotency_key_reused",
            ],
        );
    }
    if method == "patch" {
        operation["responses"]["200"]["headers"] = entry_headers;
        operation["parameters"].as_array_mut().unwrap().push(json!({
            "name":"If-Match", "in":"header", "required":true,
            "description":"The entry's ETag from a read or the last write.",
            "schema":{"type":"string","pattern":"^\"[^\"]+\"$"}
        }));
        operation["responses"]["412"] = problem(
            "The entry changed since the revision in If-Match. No mutation; read it again.",
            &["memory.conflict"],
        );
        operation["responses"]["428"] = problem(
            "If-Match is missing. No mutation.",
            &["request.if_match_required"],
        );
        operation["responses"]["409"] = problem(
            "The status change is not allowed from the entry's status, accepting or restoring finds the active quota full, or a run's edit has no claim or no write left.",
            &[
                "memory.invalid_transition",
                "memory.limit_reached",
                "memory.step_not_claimed",
                "memory.step_run_ambiguous",
                "memory.proposal_limit",
                "memory.edit_limit",
            ],
        );
        operation["responses"]["404"] = problem(
            "The entry is absent, in another project, or not visible to this credential; or restore names a pruned or unknown revision.",
            &["resource.not_found", "memory.revision_not_found"],
        );
    }
    if method == "get" {
        let query = if entry {
            vec![
                json!({"name":"revision","in":"query","description":"Read a prior revision; omitted selects current. Not available to execution credentials.","schema":{"type":"integer","minimum":1,"maximum":4294967295_u64}}),
            ]
        } else {
            vec![
                json!({"name":"q","in":"query","schema":{"type":"string"}}),
                json!({"name":"tags","in":"query","description":"Comma-separated; all must match.","schema":{"type":"string"}}),
                json!({"name":"step_id","in":"query","schema":{"type":"string"}}),
                json!({"name":"status","in":"query","schema":{"$ref":"#/components/schemas/MemoryStatus"}}),
                json!({"name":"include_expired","in":"query","schema":{"type":"boolean","default":false}}),
                json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":50}}),
                json!({"name":"cursor","in":"query","schema":{"type":"string"}}),
            ]
        };
        operation["parameters"]
            .as_array_mut()
            .unwrap()
            .extend(query);
    }
}
