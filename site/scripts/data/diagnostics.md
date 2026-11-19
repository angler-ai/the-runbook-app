<!--
Cause and fix for every diagnostic code the compiler can report.

scripts/generate.mjs combines this file with the codes it finds in the code
(the message catalog in crates/runbook-spec/src/messages.rs, through `runbook
__docs-reference`, plus every `spec.*`, `data.*` and `expression.*` literal in
the non-test sources of runbook-spec and runbook-expr) and writes
src/content/docs/reference/diagnostics.mdx. Generation fails when a code has
no entry here or an entry names a code the code no longer has.

Format: `# Section` groups codes; `## <code>` then Markdown: the cause, then
a line starting with "Fix:".
-->

# YAML and the document

## spec.yaml_syntax
The file isn't valid YAML: an indentation slip, an unclosed quote or
bracket, or a tab used for indentation.

Fix: open the line and column the diagnostic names. Quote strings that
contain `: ` or start with `{`, `[`, `*` or `&`, and expressions such as
`"${{ inputs.x == 'a' }}"`.

## spec.yaml_document_count
The file has more than one YAML document (separated by `---`).

Fix: keep one runbook per file.

## spec.yaml_feature_forbidden
The file uses anchors (`&name`), aliases (`*name`), merge keys (`<<:`) or
custom tags, which runbooks don't allow so that what you read is what
runs.

Fix: write the repeated values out, or move them to `spec.defaults`.

## spec.yaml_nesting_limit
The YAML nests deeper than the parser allows.

Fix: flatten the structure; deep nesting usually means a runaway
generator.

## spec.duplicate_key
The same key appears twice in one mapping. YAML would silently keep the
last one; runbooks refuse it.

Fix: remove or merge the duplicate.

## spec.non_string_key
A mapping key isn't a string (for example `1:` or `true:`).

Fix: quote the key.

## spec.non_json_scalar
A value can't be represented in JSON, such as `.nan`, `.inf` or a YAML
timestamp.

Fix: quote it as a string.

## spec.source_shape
The document isn't a mapping with `metadata` and `spec`.

Fix: start from a [sample](/samples/) or the shape in
[the language overview](/language/overview/).

## spec.required
A required key is missing, such as `metadata.id`, `spec.steps`, a step's
`id` or an approval's `prompt`.

Fix: add the key the diagnostic's path names.

## spec.unsupported_api_version
`apiVersion` is set to something other than `therunbook.app/v1`.

Fix: use `therunbook.app/v1`, or leave `apiVersion` out.

## spec.unsupported_kind
`kind` is set to something other than `Runbook`.

Fix: use `Runbook`, or leave `kind` out.

# Keys and shapes

## spec.unknown_key
A key the language doesn't have at that level: often a typo, or a key
nested one level too deep or too shallow.

Fix: check the spelling and level against the
[key reference](/reference/keys/). Keep your own data under a key that
starts with `x-`; those are kept and never executed.

## spec.unsupported_feature
A key the language deliberately refuses, such as `agent.runner`,
`agent.model`, `agent.max_cost_usd` or `agent.max_tokens`.

Fix: remove it. Limit what an agent can do through the access you give
it, not through the runbook.

## spec.unsupported_value
A value the language doesn't support at that key, such as an evidence kind
other than `link`, `log`, `file` or `screenshot`, or an unknown `join`.

Fix: use one of the values the [key reference](/reference/keys/) lists.

## spec.unsupported_type
A declaration's `type` isn't one of the supported types (for example
`secret_ref` or `float`).

Fix: use `string`, `text`, `number`, `integer`, `boolean`, `enum`,
`multi_enum`, `list`, `object`, `date`, `datetime`, `duration`, `url`,
`email` or `user`.

## spec.invalid_shape
A key has the wrong structure: an object where a list belongs, a `wait`
with none or several of `for`, `until` and `signal`, a loop with both
`until` and `while`, or `fail_fast` with a `join` other than `all`.

Fix: read the diagnostic's message and path, and compare with the key's
example in the [key reference](/reference/keys/).

## spec.type_error
A value has the wrong JSON type for its key: a string where a number is
expected, a list where an object is.

Fix: give the type the key expects; quote numbers you mean as strings.

## spec.invalid_value
A value is out of its allowed set or range: an unknown `actor` or
`on_failure`, a duration that doesn't parse, an empty or duplicate
approval option, a `catch_up` over 24 hours, a recipient that isn't
`team:<slug>`, `user:<name>` or a name.

Fix: use one of the documented values. Durations are digits and units,
such as `90s`, `15m`, `2h`, `1d` or `1h30m`.

## spec.invalid_node_field
A key that doesn't belong on this kind of step, such as `instructions` on
a `parallel`, `actor` on an approval or `outputs` on a group.

Fix: move the key to the step it belongs on, or turn the step into a task.
Every step takes `id`, `title`, `if`, `on_failure`, `timeout` and
`labels`.

## spec.control_conflict
A step has more than one control key, such as both `steps` and
`parallel`, or `wait` and `approval`.

Fix: split it into two steps, or nest one inside the other.

## spec.invalid_labels
A step's `labels` aren't a flat map of at most 16 strings with lowercase
keys (`[a-z0-9][a-z0-9_./-]*`, at most 63 bytes) and values of at most 256
characters.

Fix: shorten or rename the labels; put structured data under an `x-` key.

## spec.duplicate_agent_hint
A hint is given both as `agent.tools` (or `agent.commands`) and under
`agent.hints`.

Fix: keep one; `agent.hints.tools` is the current spelling.

# IDs

## spec.invalid_id
An ID has the wrong form. A runbook ID is kebab case (lowercase letters and
digits separated by single hyphens); a step or trigger ID is lowercase
letters, digits and `_`, starting with a letter. Both are at most 63
bytes.

Fix: rename it, for example `Check-Health` to `check_health` for a step.

## spec.duplicate_id
Two steps share an ID. IDs are unique across the whole runbook, including
nested and `finally` steps.

Fix: rename one of them.

## spec.duplicate_trigger
Two triggers share an ID; IDs are unique across `schedule` and `webhook`.

Fix: rename one of them.

# Steps and control flow

## spec.timeout_requires_task
`timeout` is set on a container, wait or approval step. A step's
`timeout` applies to tasks only.

Fix: put the timeout on the tasks inside, or use `wait.timeout`,
`approval.timeout` or `spec.execution.timeout`.

## spec.handoff_requires_task
`on_failure: handoff` is set on a step that isn't a task.

Fix: set it on the tasks inside the container, or under
`spec.defaults.on_failure`.

## spec.end_in_finally
An `end` step is inside a `finally` block, where the run is already
ending.

Fix: remove it, or move it to the main steps or a `catch`.

## spec.catch_on_without_catch
`catch_on` is set on a group without a `catch`.

Fix: add the `catch` steps, or remove `catch_on`.

## spec.wait_outputs_requires_signal
`wait.outputs` is set on a wait for a duration or a time. Only signals
carry a payload.

Fix: remove `outputs`, or wait for a `signal`.

## spec.followup_invalid
The reminder or escalation keys don't fit together: `remind_every`
without `remind_after`, `escalate_after` without `escalate_to` (or the
reverse), an empty `escalate_to`, or a zero duration.

Fix: give `remind_after` with `remind_every`, and `escalate_after` and
`escalate_to` together.

## spec.strict_assignment_without_assignee
`assignment: strict` is set on a task with no `assignee` to restrict to.

Fix: add an `assignee`, or remove `assignment`.

## spec.alias_collision
A `foreach.as` name shadows a name expressions already use: `inputs`,
`vars`, `fields`, `steps`, `outputs`, `index`, `iteration` or an
enclosing loop's variable.

Fix: pick another name, such as `region` or `host`.

## spec.control_depth_limit
Containers nest deeper than 32 levels.

Fix: flatten the structure, or split part of it into another runbook and
`call` it.

# Data and declarations

## spec.invalid_schema
A declaration is malformed: an `enum` without `options`, `items` on
something other than a `list`, `properties` on something other than an
`object`, a default that doesn't match its type, or `min` above `max`.

Fix: compare with the declaration examples in the
[key reference](/reference/keys/#declarations).

## spec.invalid_pattern
A `pattern` isn't a valid regular expression in the supported dialect,
which runs in linear time and has no backreferences or lookaround.

Fix: simplify the pattern; patterns already match the whole value, so
`^` and `$` aren't needed.

## spec.pattern_limit
A `pattern` is longer than 1,024 characters, or compiles too large.

Fix: shorten it, or check the value in a step instead.

## spec.depth_limit
Declarations nest deeper than 64 levels.

Fix: flatten the declared structure.

## spec.schema_limit
More than 4,096 declarations in the runbook.

Fix: declare less structure; use `object` without `properties` details
where you don't need them checked.

## spec.indexed_field_limit
More than 10 fields have `indexed: true`.

Fix: index only the fields you search runs by.

## spec.unknown_field
A `set.fields` entry, or an expression, names a field `spec.fields`
doesn't declare.

Fix: declare the field, or correct its name.

## spec.unknown_variable
A `set.vars` entry, or an expression, names a variable `spec.vars`
doesn't declare.

Fix: declare it under `spec.vars` with a starting value.

## spec.variable_cycle
Variables' starting values refer to each other in a loop (`a` uses `b`,
`b` uses `a`).

Fix: give one of them a literal starting value.

## spec.uninferable_type
A variable's starting value is `null` or an empty list, so its type can't
be inferred.

Fix: give a starting value of the type you mean, such as `""`, `0` or a
one-item list.

## data.required
A value is missing a required property: an input at start, an output in a
report, a property of an object.

Fix: supply the property the path names, or make it optional.

## data.type_error
A value doesn't match its declared type.

Fix: send the declared type; numbers you mean as text need quotes in YAML.

## data.enum
A value isn't one of the declaration's `options`.

Fix: use one of the options, or add the value to them.

## data.range
A number, a string's length or a list's size is outside `min` and `max`.

Fix: adjust the value or the bounds.

## data.pattern
A string doesn't match its declaration's `pattern`.

Fix: correct the value; remember the pattern must match the whole string.

## data.format
A value isn't in its type's format: a `date` that isn't `YYYY-MM-DD`, a
`datetime` that isn't RFC 3339, a `url` that isn't an absolute `http(s)`
URL, or an `email`, `duration` or `user` that doesn't parse.

Fix: send the value in the type's format.

## data.unknown_field
A value has a property its declaration doesn't name. Declarations are
closed.

Fix: remove the property, or declare it.

## data.depth_limit
A value nests deeper than 64 levels.

Fix: flatten the value.

# Expressions

## expression.syntax_error
An expression inside `${{ … }}` doesn't parse.

Fix: check brackets and quotes. Use single quotes for strings inside an
expression and double quotes around the whole YAML value:
`"${{ inputs.env == 'prod' }}"`.

## expression.type_error
An expression has the wrong type for where it's used (a string where `if`
needs a boolean, or two different types compared), or reads a name that
isn't in scope, such as a step that doesn't exist or a loop variable
outside its loop.

Fix: compare like with like, give `if`, `when`, `until` and `while` a
boolean expression, and check the names it reads.

## expression.unsupported
The expression uses something outside the `runbook-cel/1` profile, such
as arithmetic, string functions, list or map literals, or a macro other
than one `exists`.

Fix: compute the value in a step and read its output, or use the
operators the [expressions page](/language/expressions/) lists.

## expression.binding_error
A value bound into an expression at run time can't be represented, such
as a timestamp outside the years 0001 to 9999.

Fix: check the input or output that feeds the expression.

## expression.binding_limit
The values bound into an expression at run time are larger than the
profile allows.

Fix: read a smaller part of the data, such as one property instead of a
whole object.

## expression.compile_limit
An expression is over 256 nodes or 32 levels deep, or its source is over
4,096 bytes.

Fix: simplify it, or split it across variables.

## expression.evaluation_limit
An expression would cost more than the profile's evaluation budget.

Fix: simplify it; avoid `exists` over large lists.

## expression.integer_overflow
An integer literal overflows, such as `-(-9223372036854775808)`.

Fix: use a smaller literal.

## expression.schema_error
The types an expression can see (inputs, fields, steps and their outputs)
couldn't be assembled, usually because of a malformed declaration.

Fix: fix the declaration diagnostics first; this one usually follows from
them.

## expression.schema_limit
The runbook has more than expressions can type: over 256 steps with IDs,
over 256 declared outputs, or `foreach` and `loop` nested more than 4
deep.

Fix: split the runbook and `call` the parts, or flatten the loops.

## spec.unsupported_profile
The runbook asks for an expression profile other than `runbook-cel/1`.

Fix: remove the profile setting.

# Approvals and safety

## spec.approvers_required
An approval names no approvers. Reported when you publish.

Fix: add `approvers: { users: [...] }` or `approvers: { teams: [...] }`.

## spec.agent_approval_requires_min_one
`allow_agents: true` is set on an approval whose `min` isn't 1.

Fix: set `min: 1`, or don't let agents decide.

## spec.agent_approval_destructive
An approval that agents may decide (`allow_agents`) can lead to a
destructive task. Only people may approve a destructive path. Reported
when you publish.

Fix: remove `allow_agents`, or put a human approval between this one and
the destructive task.

## spec.auto_option_required
`on_timeout: { decide: … }` names an option that doesn't exist or isn't
marked `auto: true`.

Fix: mark the option `auto: true`, or name an option that is.

## spec.auto_decision_destructive
An approval's automatic timeout decision can lead to a destructive task
that hasn't opted in.

Fix: set `allow_auto_decision: true` on that task if an unattended
decision may really trigger it; otherwise use `on_timeout: escalate`.

## spec.escalation_recipients_required
An approval escalates at its timeout but there is no one to escalate to:
no `escalate_to` and no `metadata.owners`.

Fix: add `escalate_to`, or owners to the runbook.

## spec.on_timeout_requires_timeout
`on_timeout`, `escalate_to` or `escalation_timeout` is set on an approval
without a `timeout`.

Fix: add `timeout`, or remove the keys that depend on it.

## approval.no_escalation_target
An approval's escalation resolved to no one when it fell due.

Fix: give the approval `escalate_to` recipients who exist, or owners to
the runbook.

## approval.not_approval
An approval action named a step that isn't an approval.

Fix: use the step run ID of an approval step.

# Triggers

## spec.invalid_cron
A schedule's `cron` isn't five valid fields.

Fix: use minute, hour, day of month, month and day of week, such as
`0 2 * * *` (02:00 daily) or `*/15 9-17 * * MON-FRI`.

## spec.invalid_timezone
A schedule's `timezone` isn't an IANA zone name.

Fix: use a name such as `UTC`, `Europe/London` or
`America/Los_Angeles`, not an abbreviation such as `PST`.

## spec.trigger_input_invalid
A trigger's `inputs` name an input the runbook doesn't declare, give a
value of the wrong type, or, on a schedule, leave out a required input
that has no default.

Fix: match the runbook's `spec.inputs`.

## spec.trigger_when_type
A webhook trigger's `when` isn't a boolean expression.

Fix: compare values: `"${{ payload.status == 'success' }}"`.

# Skills

## spec.both_skill_and_skills
A step has both `skill` and `skills`.

Fix: use `skills: [a, b]` alone.

## spec.too_many_skills
A step attaches more than 8 skills.

Fix: attach the ones the step needs, or split the step.

## spec.duplicate_skill
A step lists the same skill twice in `skills`.

Fix: remove the duplicate.

## spec.unresolved_skill
A skill alias isn't declared in `spec.skills`, or (when you publish) no
skill of that name has been uploaded to the project.

Fix: declare the alias, and upload the skill with
`runbook skill push <dir> --project <project>`. Offline validation can't
check uploads; [samples](/samples/) stop here for that reason.

# Calls

## spec.call_unresolved
The called runbook has no published version (or not the pinned
`version`) in this project. Reported when you publish; offline validation
can't see your server.

Fix: publish the child runbook first, or correct its ID or version.

## spec.call_cycle
The runbook reaches itself through `call`, directly or through other
runbooks.

Fix: break the cycle; put the shared steps in a third runbook both call.

## spec.call_depth_exceeded
Calls nest more than 3 deep (a run, its child, grandchild and
great-grandchild).

Fix: flatten the chain.

## spec.call_unknown_input
`call.with` names an input the child doesn't declare.

Fix: use the child's input names.

## spec.call_missing_input
`call.with` leaves out an input the child requires and has no default.

Fix: pass it in `with`.

## spec.call_input_type
A literal in `call.with` doesn't match the child input's type.

Fix: pass the child's declared type.

## spec.call_unknown_output
`call.outputs` names an output the child doesn't have.

Fix: use the child's `spec.outputs` names.

## spec.call_output_type
`call.outputs` declares a type that doesn't match the child's output.

Fix: declare the type the child's output has.

# Directory references

## spec.unresolved_team
A team named in `approvers`, `assignee`, `escalate_to` or `owners`
doesn't exist on your server. Offline validation has no directory, so
`runbook spec validate` always stops here when a runbook names a team.

Fix: create the team (`runbook team create`), or correct its slug.

## spec.unresolved_user
A user named in the runbook doesn't exist on your server.

Fix: correct the name, or add the person.

# Limits

## spec.limit_exceeded
A bounded value is over its limit: more than 20 `parallel` branches,
`foreach.max_items` over 100, `loop.max_iterations` over 50, a
`wait.for` over 7 days, or more than 16 triggers of a kind.

Fix: lower the value, or split the work across runs.

## spec.node_limit
The runbook has more than 256 steps with IDs.

Fix: split it into runbooks joined with `call`.

## spec.source_limit
The source is larger than the server allows (1 MiB, or lower with
`RUNBOOK_MAX_SOURCE_BYTES`).

Fix: split the runbook, or move long instructions into skills.

## spec.source_value_limit
The source has more than 262,144 values.

Fix: split the runbook.

## spec.source_path_limit
The source's paths together are too long (very deep or very wide data).

Fix: flatten long data under `vars` or `with`.

## spec.compile_limit
Compiling would use more resources than allowed, usually from many large
expressions or declarations together.

Fix: split the runbook.

## spec.context_limit
The directory context sent with a compile (teams, users, skills, called
runbooks) is too large.

Fix: name fewer distinct teams, users and skills in one runbook.

## spec.step_budget_overflow
The worst-case number of step runs overflows; loops, `foreach` and
retries multiply each other.

Fix: lower `max_items`, `max_iterations` or `retry.max_attempts`, or
un-nest them.

## spec.agents_params_too_large
`spec.agents.params` encodes to more than 4,096 bytes.

Fix: pass less, and let the launcher look the rest up.

## spec.agents_dispatch_removed
`spec.agents.dispatch` was removed.

Fix: subscribe a webhook to `agent.work_available` and put what the
launcher needs in `spec.agents.params`; see [webhooks](/webhooks/).

# Internal

These come from stored, compiled runbooks rather than from what you
write. Seeing one usually means a stored version is damaged or was written
by a different release.

## spec.invalid_ir
The compiled form failed its own consistency checks.

Fix: republish the runbook; if it persists, report it with the source.

## spec.invalid_snapshot
A stored compiled snapshot couldn't be read.

Fix: republish the runbook.

## spec.snapshot_hash_mismatch
A stored snapshot doesn't belong to the source it was stored with.

Fix: republish the runbook.

## spec.snapshot_budget_mismatch
A stored snapshot's step budget doesn't match its compiled form.

Fix: republish the runbook.

## spec.snapshot_pins_mismatch
A stored snapshot's pinned skills and called runbooks don't match.

Fix: republish the runbook.

## spec.snapshot_limit
A compiled snapshot is larger than the server stores.

Fix: split the runbook.
