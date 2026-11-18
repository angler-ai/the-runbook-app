//! Presentation selection is separate from execution control flow.
use crate::ir::Instructions;
use runbook_model::ActorKind;

/// Select the checked source site, preserving explicit empty overrides.
pub fn instruction_site(
    instructions: Option<&Instructions>,
    audience: ActorKind,
) -> Option<&'static str> {
    match instructions? {
        Instructions::Markdown(_) => Some("instructions"),
        Instructions::Audience {
            default,
            human,
            agent,
        } => match audience {
            ActorKind::Human if human.is_some() => Some("instructions/human"),
            ActorKind::Agent if agent.is_some() => Some("instructions/agent"),
            _ if default.is_some() => Some("instructions/default"),
            _ => None,
        },
    }
}
