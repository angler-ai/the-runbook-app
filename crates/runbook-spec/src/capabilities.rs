use serde_json::Value;

/// Capability promise for the completed pilot, not current runtime readiness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Supported { owner: String },
    Preserved { reason: String },
    Rejected { code: String },
}

impl Support {
    /// Migration guidance when a formerly accepted capability was removed.
    pub fn message(&self) -> Option<&'static str> {
        match self.code() {
            Some("spec.agents_dispatch_removed") => Some(
                "Use project webhook subscriptions for agent dispatch and spec.agents.params for opaque subscriber parameters.",
            ),
            Some("spec.retired_key") => Some(
                "agent.runner is retired: name the runner agent with agent.name (agent.name: external keeps the step open to external agents).",
            ),
            _ => None,
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Rejected { code } => Some(code),
            _ => None,
        }
    }
}

// Context matters: a property named `pattern` is legal user data, while the
// declaration keyword `pattern` is a deferred type constraint.
#[derive(Clone, Copy)]
enum Context {
    Root,
    Metadata,
    Body,
    Execution,
    Concurrency,
    Defaults,
    Retry,
    Declarations(bool),
    Declaration(bool),
    Agents,
    AgentParams,
    AgentParamsData,
    RemovedDispatch,
    Nodes(&'static str),
    Node,
    Instructions,
    Evidence,
    EvidenceKinds,
    EvidenceKind,
    Agent,
    AgentHints,
    DefaultsAgent,
    Retired,
    Triggers,
    Schedules,
    Schedule,
    Webhooks,
    Webhook,
    Foreach,
    Loop,
    Switch,
    Cases,
    Case,
    Branch,
    Wait,
    Approval,
    ApprovalAgent,
    Options,
    Option,
    Approvers,
    TimeoutPolicy,
    Set,
    End,
    Call,
    Version,
    Kind,
    Type,
    Queue,
    Join,
    Atom(&'static str),
    Data(&'static str),
    Advisory,
    Extension,
}

impl Context {
    fn owner(self) -> &'static str {
        use Context::*;
        match self {
            Root | Version | Kind | Metadata => "A1/A5/B1/B3",
            Body | Node => "A7-A20/B8",
            Execution => "A4/A18/A19/B5",
            Concurrency | Queue => "B5",
            Defaults | Retry => "A5/A15/A18",
            Declarations(_) | Declaration(_) | Type => "A4/A5",
            Agents | AgentParams | AgentParamsData => "C1",
            Instructions => "A20",
            Evidence | EvidenceKinds | EvidenceKind => "A8/B8",
            Agent => "A5",
            AgentHints => "LANG9",
            DefaultsAgent | ApprovalAgent => "R0",
            Triggers | Schedules | Schedule | Webhooks | Webhook => "LANG9b",
            Foreach => "A12",
            Loop => "A13",
            Switch | Cases | Case | Branch => "A10",
            Wait => "A17",
            Approval => "A6/A16/A18/B1/B6",
            Options | Option | Approvers | TimeoutPolicy => "A6/A16",
            Set => "A9",
            End => "A9/A19",
            Call => "LANG4",
            Join => "A11",
            Atom(owner) | Data(owner) | Nodes(owner) => owner,
            Advisory | Extension | RemovedDispatch | Retired => unreachable!(),
        }
    }

    fn child(self, key: &str) -> std::option::Option<Self> {
        use Context::*;
        let index = key == "*" || key.parse::<usize>().is_ok();
        if matches!(self, AgentParams | AgentParamsData) {
            return Some(AgentParamsData);
        }
        // Extensions are opaque throughout their subtree, at every object level.
        if key.starts_with("x-") || matches!(self, Extension) {
            return Some(Extension);
        }
        if matches!(self, Advisory | Data(_)) {
            return Some(self);
        }
        Some(match (self, key) {
            (Root, "apiVersion") => Version,
            (Root, "kind") => Kind,
            (Root, "metadata") => Metadata,
            (Root, "spec") => Body,
            (Metadata, "id" | "title" | "description") => Atom("A1/A5/B1/B3"),
            (Metadata, "tags" | "owners") => Data("A1/A5/B1/B3"),
            (Body, "execution") => Execution,
            (Body, "inputs") => Declarations(false),
            (Body, "fields") => Declarations(true),
            (Body, "vars") => Data("A4/A5"),
            (Body, "defaults") => Defaults,
            (Body, "skills") => Data("A4/A5/A20/B3"),
            (Body, "agents") => Agents,
            (Body, "triggers") => Triggers,
            (Triggers, "schedule") => Schedules,
            (Triggers, "webhook") => Webhooks,
            (Schedules, _) if index => Schedule,
            (Webhooks, _) if index => Webhook,
            (Schedule, "id" | "cron" | "timezone" | "catch_up") => Atom("LANG9b"),
            (Schedule, "inputs") => Data("LANG9b"),
            (Webhook, "id" | "when" | "dedupe") => Atom("LANG9b"),
            (Webhook, "inputs") => Data("LANG9b"),
            (Webhook, "payload") => Declarations(false),
            (Body, "steps" | "finally") => Nodes("A7-A20/B8"),
            (Body, "outputs") => Data("A7-A20/B8"),
            (Body, "report") => Atom("A20/B8"),
            (Execution, "title" | "tags" | "timeout" | "cleanup_timeout") => Data("A4/A18/A19/B5"),
            (Execution, "concurrency") => Concurrency,
            (Concurrency, "key" | "limit") => Atom("B5"),
            (Concurrency, "on_conflict") => Queue,
            (Declarations(field), _) => Declaration(field),
            (Declaration(_), "type") => Type,
            (Declaration(_), "title" | "description" | "required" | "min" | "max") => Atom("A4/A5"),
            (Declaration(_), "default" | "options") => Data("A4/A5"),
            (Declaration(_), "items") => Declaration(false),
            (Declaration(_), "properties") => Declarations(false),
            (Declaration(true), "indexed" | "locked") => Atom("B3/B8"),
            (Declaration(_), "pattern") => Atom("A4/A5/LANG5"),
            (Defaults, "actor" | "timeout" | "on_failure") => Atom("A5/A15/A18"),
            (Defaults, "agent") => DefaultsAgent,
            (DefaultsAgent, "name") => Atom("R0"),
            (Defaults | Node, "remind_after" | "remind_every" | "escalate_after") => Atom("LANG9"),
            (Defaults | Node, "escalate_to") => Data("LANG9"),
            (Defaults | Node, "retry") => Retry,
            (Retry, "max_attempts" | "backoff") => Atom("A5/A15/A18"),
            (Retry, "on") => Data("LANG9"),
            (Node, "catch_on") => Data("LANG9"),
            (Agents, "params") => AgentParams,
            (Agents, "dispatch") => RemovedDispatch,
            (Nodes(_), _) if index => Node,
            (
                Node,
                "id"
                | "title"
                | "if"
                | "actor"
                | "assignee"
                | "check"
                | "skippable"
                | "timeout"
                | "on_failure"
                | "destructive"
                | "allow_auto_decision",
            ) => Atom("A5/A8/A15-A20/B1/B6"),
            (Node, "assignment") => Atom("LANG7"),
            (Node, "instructions") => Instructions,
            (Instructions, "default" | "human" | "agent") => Atom("A20"),
            (Node, "skill") => Atom("A4/A5/A20/B3"),
            (Node, "skills") => Data("LANG8"),
            (Node, "labels") => Data("LANG9"),
            (Node, "with") => Data("A4/A5/A20/B3"),
            (Node, "checklist") => Data("A8"),
            (Node, "outputs") => Declarations(false),
            (Node, "evidence") => Evidence,
            (Evidence, "required" | "accepted") => EvidenceKinds,
            (EvidenceKinds, _) if index => EvidenceKind,
            (Node, "agent") => Agent,
            (Agent, "tools" | "commands") => Advisory,
            // LANG9: the explicit home for hints. Everything in it is passed to
            // the agent and never enforced; `agent.tools` and `agent.commands`
            // stay accepted as the older spellings.
            (Agent, "hints") => AgentHints,
            (AgentHints, "tools" | "commands") => Advisory,
            // R0-3: runner agent keys; `agent.runner` is retired.
            (Agent, "name" | "model" | "max_cost_usd" | "max_tokens") => Atom("R0"),
            (Agent, "network") => Data("R0"),
            (Agent, "runner") => Retired,
            (Node, "connections") => Data("R0"),
            (Node, "steps" | "catch" | "finally") => Nodes("A7/A14/A19"),
            (Node, "parallel") => Nodes("A11"),
            (Node, "join") => Join,
            (Join, "at_least") => Atom("A11/LANG2"),
            (Node, "fail_fast") => Atom("A11"),
            (Node, "foreach") => Foreach,
            (Foreach, "items" | "as" | "key" | "concurrency" | "max_items") => Atom("A12"),
            (Foreach, "steps") => Nodes("A12"),
            (Node, "loop") => Loop,
            (Loop, "until" | "max_iterations" | "delay") => Atom("A13"),
            (Loop, "steps") => Nodes("A13"),
            (Loop, "while") => Atom("A13/LANG1"),
            (Node, "switch") => Switch,
            (Switch, "cases") => Cases,
            (Cases, _) if index => Case,
            (Case, "when") => Atom("A10"),
            (Case | Branch, "steps") => Nodes("A10"),
            (Switch, "default") => Branch,
            (Node, "wait") => Wait,
            (Wait, "for" | "until" | "signal" | "timeout") => Atom("A17"),
            (Wait, "outputs") => Declarations(false),
            (Node, "approval") => Approval,
            (
                Approval,
                "prompt" | "min" | "timeout" | "escalation_timeout" | "on_expired" | "allow_agents",
            ) => Atom("A6/A16/A18/B1/B6"),
            (Approval, "escalate_to" | "approves") => Data("A6/A16/A18/B1/B6"),
            (Approval, "agent") => ApprovalAgent,
            (ApprovalAgent, "name" | "model" | "max_cost_usd" | "max_tokens" | "after") => {
                Atom("R0")
            }
            (Approval, "options") => Options,
            (Options, _) if index => Option,
            (Option, "id" | "title" | "auto") => Atom("A6/A16"),
            (Approval, "approvers") => Approvers,
            (Approvers, "users" | "teams") => Data("A6/A16"),
            (Approval, "on_timeout") => TimeoutPolicy,
            (TimeoutPolicy, "decide") => Atom("A6/A16"),
            (Node, "set") => Set,
            (Set, "vars" | "fields") => Data("A9"),
            (Node, "end") => End,
            (End, "status" | "message") => Atom("A9/A19"),
            (Node, "call") => Call,
            (Call, "runbook" | "version") => Atom("LANG4"),
            (Call, "with") => Data("LANG4"),
            (Call, "outputs") => Declarations(false),
            _ => return None,
        })
    }
}

fn rejected(code: &str) -> Support {
    Support::Rejected { code: code.into() }
}

fn params_fit(root: &Value) -> bool {
    crate::size::measure(root, 4096, usize::MAX).is_ok()
}

/// Classify a source path as a promise for the finished pilot.
///
/// Accepts RFC 6901 pointers (unambiguous for arbitrary map keys) or dotted
/// paths with array indices such as `spec.steps[0].outputs.value.type`.
/// `*`/`[]` can stand for an array index or a schema map entry.
/// This checks capability availability, not general shape or type validity;
/// structural validation and expression compilation are separate passes.
pub fn classify(path: &str, value: &Value) -> Support {
    use Context::*;
    let parts: Vec<String> = if let Some(pointer) = path.strip_prefix('/') {
        pointer
            .split('/')
            .map(|p| p.replace("~1", "/").replace("~0", "~"))
            .collect()
    } else if path.is_empty() {
        Vec::new()
    } else {
        path.replace("[]", ".*.")
            .replace(['[', ']'], ".")
            .split('.')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let mut context = Root;
    for part in parts {
        context = match context.child(&part) {
            Some(RemovedDispatch) => return rejected("spec.agents_dispatch_removed"),
            Some(Retired) => return rejected("spec.retired_key"),
            Some(next) => next,
            None => return rejected("spec.unknown_key"),
        };
    }
    match context {
        Extension => {
            return Support::Preserved {
                reason: "Extension data is retained verbatim and never executed".into(),
            };
        }
        Advisory => {
            return Support::Preserved {
                reason: "Advisory agent context is exposed; the server does not execute it".into(),
            };
        }
        AgentParams if !value.is_object() => return rejected("spec.type_error"),
        AgentParams if !params_fit(value) => return rejected("spec.agents_params_too_large"),
        Version if value != "therunbook.app/v1" => return rejected("spec.unsupported_api_version"),
        Kind if value != "Runbook" => return rejected("spec.unsupported_kind"),
        Type if !matches!(
            value.as_str(),
            Some(
                "string"
                    | "text"
                    | "number"
                    | "integer"
                    | "boolean"
                    | "enum"
                    | "multi_enum"
                    | "list"
                    | "object"
                    | "date"
                    | "datetime"
                    | "duration"
                    | "url"
                    | "email"
                    | "user"
            )
        ) =>
        {
            return rejected("spec.unsupported_type");
        }
        Type if matches!(
            value.as_str(),
            Some("multi_enum" | "date" | "datetime" | "duration" | "url" | "email" | "user")
        ) =>
        {
            return Support::Supported {
                owner: "A4/A5/LANG5".into(),
            };
        }
        // LANG3: `reject` and `cancel_previous` (PRD §6 "Concurrency").
        Queue if value == "reject" || value == "cancel_previous" => {
            return Support::Supported {
                owner: "B5/LANG3".into(),
            };
        }
        Queue if value != "queue" => return rejected("spec.unsupported_value"),
        // LANG2: `any` and `{at_least: n}` (its members are classified as
        // children); other values are refused.
        Join if value == "any" || value.is_object() => {
            return Support::Supported {
                owner: "A11/LANG2".into(),
            };
        }
        Join if value != "all" => return rejected("spec.unsupported_value"),
        EvidenceKind if !matches!(value.as_str(), Some("link" | "log" | "file" | "screenshot")) => {
            return rejected("spec.unsupported_value");
        }
        // UP1: uploaded evidence, finalized by size and magic bytes (D49).
        EvidenceKind if matches!(value.as_str(), Some("file" | "screenshot")) => {
            return Support::Supported {
                owner: "A8/B8/UP1".into(),
            };
        }
        _ => {}
    }
    Support::Supported {
        owner: context.owner().into(),
    }
}

/// Report rejected pilot capabilities against the original source locations.
///
/// Preserved subtrees are opaque; rejected containers produce one diagnostic
/// rather than a cascade for their children. This never mutates source data.
pub fn validate_capabilities(spec: &crate::Spec) -> Vec<crate::Diagnostic> {
    fn visit(
        spec: &crate::Spec,
        path: &str,
        value: &Value,
        diagnostics: &mut Vec<crate::Diagnostic>,
    ) {
        let support = classify(path, value);
        let message = support.message().map(str::to_owned);
        match support {
            Support::Rejected { code } => {
                let span = spec
                    .source_spans
                    .get(path)
                    .copied()
                    .unwrap_or(crate::SourceSpan { line: 1, column: 1 });
                diagnostics.push(crate::Diagnostic {
                    message,
                    code,
                    path: path.into(),
                    line: span.line,
                    column: span.column,
                });
            }
            Support::Preserved { .. } => {}
            Support::Supported { .. } if path == "/spec/agents/params" => {}
            Support::Supported { .. } => match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        let key = key.replace('~', "~0").replace('/', "~1");
                        visit(spec, &format!("{path}/{key}"), child, diagnostics);
                    }
                }
                Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        visit(spec, &format!("{path}/{index}"), child, diagnostics);
                    }
                }
                _ => {}
            },
        }
    }
    let mut diagnostics = Vec::new();
    visit(spec, "", spec.source_json(), &mut diagnostics);
    diagnostics
}
