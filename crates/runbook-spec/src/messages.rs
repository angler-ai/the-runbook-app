//! Diagnostic message catalog for compiler errors.

/// Returns a human-readable English sentence explaining a compiler diagnostic code.
pub fn message_for(code: &str, path: &str) -> String {
    let at_path = !path.is_empty();
    match code {
        "approval.no_escalation_target" => {
            if at_path {
                format!(
                    "Approval escalation at {path} must specify at least one user or team recipient."
                )
            } else {
                "Approval escalation must specify at least one user or team recipient.".into()
            }
        }
        "approval.not_approval" => {
            if at_path {
                format!("Step at {path} is not an approval step.")
            } else {
                "This step is not an approval step.".into()
            }
        }
        "data.depth_limit" => {
            if at_path {
                format!("Data nesting at {path} exceeds the maximum allowed depth.")
            } else {
                "Data nesting exceeds the maximum allowed depth.".into()
            }
        }
        "data.enum" => {
            if at_path {
                format!("The value at {path} is not one of the allowed enum options.")
            } else {
                "The value is not one of the allowed enum options.".into()
            }
        }
        "data.range" => {
            if at_path {
                format!("The numeric value at {path} is outside the allowed range.")
            } else {
                "The numeric value is outside the allowed range.".into()
            }
        }
        "data.required" => {
            if at_path {
                format!("Required field at {path} is missing.")
            } else {
                "A required field is missing.".into()
            }
        }
        "data.type_error" => {
            if at_path {
                format!("The value at {path} does not match the expected data type.")
            } else {
                "The value does not match the expected data type.".into()
            }
        }
        "data.unknown_field" => {
            if at_path {
                format!("Unknown field at {path} is not defined in the schema.")
            } else {
                "Unknown field is not defined in the schema.".into()
            }
        }
        "expression.schema_error" => {
            if at_path {
                format!("Expression schema at {path} is invalid.")
            } else {
                "Expression schema is invalid.".into()
            }
        }
        "expression.schema_limit" => {
            if at_path {
                format!("Expression schema at {path} exceeds the maximum allowed types or fields.")
            } else {
                "Expression schema exceeds the maximum allowed types or fields.".into()
            }
        }
        "expression.type_error" => {
            if at_path {
                format!("The expression at {path} has the wrong type for where it is used.")
            } else {
                "The expression has the wrong type for where it is used.".into()
            }
        }
        "spec.agent_approval_destructive" => {
            if at_path {
                format!(
                    "The approval at {path} lets agents decide but leads to a destructive task; only people may approve that path."
                )
            } else {
                "An approval that agents may decide leads to a destructive task; only people may approve that path.".into()
            }
        }
        "spec.agent_approval_requires_min_one" => {
            if at_path {
                format!("Agent approvals at {path} require min to be set to 1.")
            } else {
                "Agent approvals require min to be set to 1.".into()
            }
        }
        "spec.agent_unknown" => {
            if at_path {
                format!("No runner agent with the name at {path} exists in this project.")
            } else {
                "No runner agent with this name exists in this project.".into()
            }
        }
        "spec.agent_outside_ceiling" => {
            if at_path {
                format!("The value at {path} is outside what the runner agent allows.")
            } else {
                "This step asks for more than its runner agent allows.".into()
            }
        }
        "spec.agent_requires_allow_agents" => {
            "A runner target requires allow_agents: true on this approval.".into()
        }
        "spec.agent_requires_agent_actor" => {
            if at_path {
                format!("The agent named at {path} needs a step with actor: agent.")
            } else {
                "Naming a runner agent needs a step with actor: agent.".into()
            }
        }
        "spec.destructive_reach" => {
            if at_path {
                format!(
                    "The step at {path} can use destructive tools or credentials: mark it destructive or narrow its tools."
                )
            } else {
                "This step can use destructive tools or credentials: mark it destructive or narrow its tools.".into()
            }
        }
        "spec.destructive_needs_approval" => {
            if at_path {
                format!(
                    "The destructive runner step at {path} must come after a person's approval on every path to it."
                )
            } else {
                "A destructive runner step must come after a person's approval on every path to it."
                    .into()
            }
        }
        "spec.retired_key" => {
            if at_path {
                format!("The key at {path} is retired; use agent.name to name a runner agent.")
            } else {
                "This key is retired; use agent.name to name a runner agent.".into()
            }
        }
        "spec.agents_dispatch_removed" => {
            if at_path {
                format!(
                    "Agent dispatch at {path} is no longer supported; use webhook subscriptions instead."
                )
            } else {
                "Agent dispatch is no longer supported; use webhook subscriptions instead.".into()
            }
        }
        "spec.agents_params_too_large" => {
            if at_path {
                format!("Agent parameters at {path} exceed the allowed size limit.")
            } else {
                "Agent parameters exceed the allowed size limit.".into()
            }
        }
        "spec.alias_collision" => {
            if at_path {
                format!("Loop alias at {path} collides with a reserved or existing name.")
            } else {
                "Loop alias collides with a reserved or existing name.".into()
            }
        }
        "spec.approvers_required" => {
            if at_path {
                format!("Approval at {path} must specify at least one approver.")
            } else {
                "Approval must specify at least one approver.".into()
            }
        }
        "spec.auto_decision_destructive" => {
            if at_path {
                format!(
                    "Automatic decision at {path} cannot reach a destructive task without allow_auto_decision."
                )
            } else {
                "Automatic decision cannot reach a destructive task without allow_auto_decision."
                    .into()
            }
        }
        "spec.auto_option_required" => {
            if at_path {
                format!("Automatic timeout decision at {path} must name an option with auto: true.")
            } else {
                "Automatic timeout decision must name an option with auto: true.".into()
            }
        }
        "spec.both_skill_and_skills" => {
            if at_path {
                format!(
                    "Conflicting skill declarations at {path}; define either 'skill' or 'skills', not both."
                )
            } else {
                "A step cannot define both 'skill' and 'skills'.".into()
            }
        }
        "spec.duplicate_agent_hint" => {
            if at_path {
                format!(
                    "The agent hint at {path} is also given directly under 'agent'; give it once, preferably under 'agent.hints'."
                )
            } else {
                "An agent hint is given both under 'agent' and under 'agent.hints'; give it once."
                    .into()
            }
        }
        "spec.invalid_labels" => {
            if at_path {
                format!(
                    "Invalid labels at {path}: use at most 16 labels, keys of lowercase letters, digits, '_', '.', '/' or '-' (at most 63 characters), and string values of at most 256 characters."
                )
            } else {
                "Invalid step labels.".into()
            }
        }
        "spec.wait_outputs_requires_signal" => {
            if at_path {
                format!(
                    "Outputs at {path} need a signal wait: only a signal carries a payload to check."
                )
            } else {
                "Wait outputs need a signal wait.".into()
            }
        }
        "spec.catch_on_without_catch" => {
            if at_path {
                format!("catch_on at {path} needs a catch block on the same group.")
            } else {
                "catch_on needs a catch block on the same group.".into()
            }
        }
        "spec.followup_invalid" => {
            if at_path {
                format!(
                    "Invalid follow-up at {path}: durations must be positive, remind_every needs remind_after, and escalate_after and escalate_to go together."
                )
            } else {
                "Invalid reminder or escalation settings.".into()
            }
        }
        "spec.duplicate_trigger" => {
            if at_path {
                format!(
                    "Duplicate trigger ID at {path}: schedule and webhook trigger IDs are unique across both."
                )
            } else {
                "Duplicate trigger ID.".into()
            }
        }
        "spec.invalid_cron" => {
            if at_path {
                format!(
                    "Invalid cron expression at {path}: use five fields (minute hour day-of-month month day-of-week), such as \"0 2 * * *\"."
                )
            } else {
                "Invalid cron expression.".into()
            }
        }
        "spec.invalid_timezone" => {
            if at_path {
                format!(
                    "Unknown time zone at {path}: use an IANA name such as UTC or America/Los_Angeles."
                )
            } else {
                "Unknown time zone.".into()
            }
        }
        "spec.trigger_input_invalid" => {
            if at_path {
                format!(
                    "Invalid trigger input at {path}: it must name a declared input with a value of its type, and a schedule must give every required input without a default."
                )
            } else {
                "Invalid trigger inputs.".into()
            }
        }
        "spec.trigger_when_type" => {
            if at_path {
                format!(
                    "A trigger's when at {path} must be a single expression that is true or false."
                )
            } else {
                "A trigger's when must be true or false.".into()
            }
        }
        "spec.compile_limit" => {
            if at_path {
                format!("Compilation resource limit exceeded at {path}.")
            } else {
                "Compilation resource limit exceeded.".into()
            }
        }
        "spec.context_limit" => {
            if at_path {
                format!("Compilation context at {path} exceeds the allowed size limit.")
            } else {
                "Compilation context exceeds the allowed size limit.".into()
            }
        }
        "spec.control_conflict" => {
            if at_path {
                format!("Multiple conflicting control flow structures are defined at {path}.")
            } else {
                "Multiple conflicting control flow structures are defined on this step.".into()
            }
        }
        "spec.control_depth_limit" => {
            if at_path {
                format!("Control flow nesting at {path} exceeds the maximum allowed depth.")
            } else {
                "Control flow nesting exceeds the maximum allowed depth.".into()
            }
        }
        "spec.depth_limit" => {
            if at_path {
                format!("Schema declaration nesting at {path} exceeds the maximum allowed depth.")
            } else {
                "Schema declaration nesting exceeds the maximum allowed depth.".into()
            }
        }
        "spec.duplicate_id" => {
            if at_path {
                format!("Step ID at {path} is already used by another step.")
            } else {
                "Duplicate step ID found in the runbook.".into()
            }
        }
        "spec.duplicate_key" => {
            if at_path {
                format!("Duplicate key found in mapping at {path}.")
            } else {
                "Duplicate key found in mapping.".into()
            }
        }
        "spec.duplicate_skill" => {
            if at_path {
                format!("Duplicate skill declared at {path}.")
            } else {
                "A step cannot declare duplicate skills.".into()
            }
        }
        "spec.end_in_finally" => {
            if at_path {
                format!("End steps are not permitted inside finally blocks at {path}.")
            } else {
                "End steps are not permitted inside finally blocks.".into()
            }
        }
        "spec.escalation_recipients_required" => {
            if at_path {
                format!("Escalation policy at {path} must specify at least one recipient.")
            } else {
                "Escalation policy must specify at least one recipient.".into()
            }
        }
        "spec.handoff_requires_task" => {
            if at_path {
                format!("Failure handoff at {path} is only supported on task steps.")
            } else {
                "Failure handoff is only supported on task steps.".into()
            }
        }
        "spec.indexed_field_limit" => {
            if at_path {
                format!("The number of indexed fields at {path} exceeds the limit of 10.")
            } else {
                "The number of indexed fields exceeds the limit of 10.".into()
            }
        }
        "spec.invalid_id" => {
            if at_path {
                format!(
                    "Identifier at {path} contains invalid characters or has an invalid length."
                )
            } else {
                "Identifier contains invalid characters or has an invalid length.".into()
            }
        }
        "spec.invalid_ir" => {
            if at_path {
                format!("Intermediate representation at {path} is invalid.")
            } else {
                "Intermediate representation is invalid.".into()
            }
        }
        "spec.invalid_node_field" => {
            if at_path {
                format!("Field at {path} is not valid for this step type.")
            } else {
                "Field is not valid for this step type.".into()
            }
        }
        "spec.invalid_schema" => {
            if at_path {
                format!("Schema declaration at {path} is invalid.")
            } else {
                "Schema declaration is invalid.".into()
            }
        }
        "spec.invalid_shape" => {
            if at_path {
                format!("The structure at {path} does not have the expected object or array shape.")
            } else {
                "The structure does not have the expected object or array shape.".into()
            }
        }
        "spec.invalid_snapshot" => {
            if at_path {
                format!("Compiled snapshot at {path} is corrupted or invalid.")
            } else {
                "Compiled snapshot is corrupted or invalid.".into()
            }
        }
        "spec.invalid_value" => {
            if at_path {
                format!("The value at {path} is not allowed here.")
            } else {
                "The value is not allowed here.".into()
            }
        }
        "spec.limit_exceeded" => {
            if at_path {
                format!("The value at {path} exceeds the maximum allowed limit.")
            } else {
                "The value exceeds the maximum allowed limit.".into()
            }
        }
        "spec.node_limit" => {
            if at_path {
                format!("Runbook at {path} exceeds the limit of 256 steps.")
            } else {
                "Runbook exceeds the limit of 256 steps.".into()
            }
        }
        "spec.non_json_scalar" => {
            if at_path {
                format!("Scalar value at {path} is not a valid JSON number or scalar.")
            } else {
                "Scalar value is not a valid JSON number or scalar.".into()
            }
        }
        "spec.non_string_key" => {
            if at_path {
                format!("Mapping key at {path} must be a string.")
            } else {
                "Mapping keys must be strings.".into()
            }
        }
        "spec.on_timeout_requires_timeout" => {
            if at_path {
                format!("Field {path} requires timeout to be specified.")
            } else {
                "Setting on_timeout requires timeout to be specified.".into()
            }
        }
        "spec.required" => {
            if at_path {
                format!("Required field at {path} is missing.")
            } else {
                "A required field is missing.".into()
            }
        }
        "spec.schema_limit" => {
            if at_path {
                format!("Schema declaration count at {path} exceeds the maximum limit.")
            } else {
                "Schema declaration count exceeds the maximum limit.".into()
            }
        }
        "spec.snapshot_budget_mismatch" => {
            if at_path {
                format!("Snapshot step budget at {path} does not match the compiled runbook.")
            } else {
                "Snapshot step budget does not match the compiled runbook.".into()
            }
        }
        "spec.snapshot_hash_mismatch" => {
            if at_path {
                format!("Snapshot hash at {path} does not match the runbook source hash.")
            } else {
                "Snapshot hash does not match the runbook source hash.".into()
            }
        }
        "spec.snapshot_limit" => {
            if at_path {
                format!("Compiled snapshot at {path} exceeds the size limit.")
            } else {
                "Compiled snapshot exceeds the size limit.".into()
            }
        }
        "spec.snapshot_pins_mismatch" => {
            if at_path {
                format!("Snapshot resource pins at {path} do not match the environment.")
            } else {
                "Snapshot resource pins do not match the environment.".into()
            }
        }
        "spec.source_limit" => {
            if at_path {
                format!("Runbook source at {path} exceeds the size limit.")
            } else {
                "Runbook source exceeds the size limit.".into()
            }
        }
        "spec.source_path_limit" => {
            if at_path {
                format!("Cumulative source path bytes at {path} exceed the allowed budget.")
            } else {
                "Cumulative source path bytes exceed the allowed budget.".into()
            }
        }
        "spec.source_shape" => {
            if at_path {
                format!("Document structure at {path} does not match the runbook schema.")
            } else {
                "Document structure does not match the runbook schema.".into()
            }
        }
        "spec.source_value_limit" => {
            if at_path {
                format!("Total number of source values at {path} exceeds the maximum limit.")
            } else {
                "Total number of source values exceeds the maximum limit.".into()
            }
        }
        "spec.step_budget_overflow" => {
            if at_path {
                format!("Worst-case step budget calculation overflowed at {path}.")
            } else {
                "Worst-case step budget calculation overflowed.".into()
            }
        }
        "spec.too_many_skills" => {
            if at_path {
                format!(
                    "Too many skills declared at {path}; at most 8 skills are allowed per step."
                )
            } else {
                "A step can define at most 8 skills.".into()
            }
        }
        "spec.timeout_requires_task" => {
            if at_path {
                format!("Timeout setting at {path} is only supported on task steps.")
            } else {
                "Timeout setting is only supported on task steps.".into()
            }
        }
        "spec.type_error" => {
            if at_path {
                format!("The value at {path} has an invalid type.")
            } else {
                "The value has an invalid type.".into()
            }
        }
        "spec.uninferable_type" => {
            if at_path {
                format!("Type cannot be inferred at {path}; specify an explicit schema type.")
            } else {
                "Type cannot be inferred from null or an empty list.".into()
            }
        }
        "spec.unknown_field" => {
            if at_path {
                format!("Referenced field at {path} is not defined.")
            } else {
                "Referenced field is not defined.".into()
            }
        }
        "spec.unknown_key" => {
            if at_path {
                format!("Unknown key at {path} is not recognized.")
            } else {
                "Unknown key is not recognized.".into()
            }
        }
        "spec.unknown_variable" => {
            if at_path {
                format!("Variable referenced at {path} is not declared.")
            } else {
                "Referenced variable is not declared.".into()
            }
        }
        "spec.unresolved_skill" => {
            if at_path {
                format!("Skill reference at {path} could not be resolved in the skill catalog.")
            } else {
                "Skill reference could not be resolved in the skill catalog.".into()
            }
        }
        "spec.unresolved_team" => {
            if at_path {
                format!("Team reference at {path} could not be resolved.")
            } else {
                "Team reference could not be resolved.".into()
            }
        }
        "spec.unresolved_user" => {
            if at_path {
                format!("User reference at {path} could not be resolved.")
            } else {
                "User reference could not be resolved.".into()
            }
        }
        "spec.unsupported_api_version" => {
            if at_path {
                format!("API version at {path} is not supported; use therunbook.app/v1.")
            } else {
                "API version is not supported; use therunbook.app/v1.".into()
            }
        }
        "spec.unsupported_feature" => {
            if at_path {
                format!("Feature used at {path} is not supported in this profile.")
            } else {
                "This feature is not supported in this profile.".into()
            }
        }
        "spec.unsupported_kind" => {
            if at_path {
                format!("Document kind at {path} is not supported; must be Runbook.")
            } else {
                "Document kind is not supported; must be Runbook.".into()
            }
        }
        "spec.unsupported_profile" => {
            if at_path {
                format!("Expression profile at {path} is not supported.")
            } else {
                "Expression profile is not supported.".into()
            }
        }
        "spec.unsupported_type" => {
            if at_path {
                format!("Data type at {path} is not supported.")
            } else {
                "Data type is not supported.".into()
            }
        }
        "spec.unsupported_value" => {
            if at_path {
                format!("Value at {path} is not supported.")
            } else {
                "The value is not supported.".into()
            }
        }
        "spec.variable_cycle" => {
            if at_path {
                format!("Circular dependency detected in variable declarations at {path}.")
            } else {
                "Circular dependency detected in variable declarations.".into()
            }
        }
        "spec.yaml_document_count" => {
            if at_path {
                format!(
                    "Multiple YAML documents found at {path}; only single-document runbooks are supported."
                )
            } else {
                "Multiple YAML documents found; only single-document runbooks are supported.".into()
            }
        }
        "spec.yaml_feature_forbidden" => {
            if at_path {
                format!(
                    "Forbidden YAML feature (such as anchors, aliases, or merge keys) used at {path}."
                )
            } else {
                "Forbidden YAML feature (such as anchors, aliases, or merge keys) used in document."
                    .into()
            }
        }
        "spec.yaml_nesting_limit" => {
            if at_path {
                format!("YAML nesting depth at {path} exceeds the maximum limit.")
            } else {
                "YAML nesting depth exceeds the maximum limit.".into()
            }
        }
        "spec.yaml_syntax" => {
            if at_path {
                format!("YAML syntax error at {path}.")
            } else {
                "YAML syntax error in runbook source.".into()
            }
        }
        _ => {
            if at_path {
                format!("Problem at {path} ({code}).")
            } else {
                format!("Problem in the runbook ({code}).")
            }
        }
    }
}
