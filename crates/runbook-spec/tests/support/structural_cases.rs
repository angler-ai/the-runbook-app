use serde_json::{Value, json};
pub fn cases() -> Vec<(Value, bool)> {
    let doc = |body| json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":body});
    vec![
        (doc(json!({"steps":[]})), true),
        (
            doc(
                json!({"inputs":{"name":{"type":"string"}},"steps":[{"id":"task","title":"Task","checklist":["One"]}]}),
            ),
            true,
        ),
        (
            doc(json!({"steps":[{"id":"group","steps":[],"catch":[],"finally":[]}]})),
            true,
        ),
        (
            doc(json!({"steps":[{"id":"wait","wait":{"signal":"go"}}]})),
            true,
        ),
        // LANG7: a task title is derived from its ID when absent.
        (doc(json!({"steps":[{"id":"task"}]})), true),
        (
            json!({"metadata":{"id":"test"},"spec":{"steps":[{"id":"task"}]}}),
            true,
        ),
        (json!({"spec":{"steps":[]}}), false),
        (
            doc(
                json!({"inputs":{"owner":{"type":"string"}},"steps":[{"id":"task","title":"Task","assignee":"${{ inputs.owner }}","assignment":"strict"}]}),
            ),
            true,
        ),
        (
            doc(json!({"steps":[{"id":"task","title":"Task","assignment":"strict"}]})),
            false,
        ),
        (
            doc(
                json!({"steps":[{"id":"task","title":"Task","assignee":"team:oncall","assignment":"maybe"}]}),
            ),
            false,
        ),
        (doc(json!({"steps":[{"id":"task","title":7}]})), false),
        (doc(json!({"steps":[],"unknown":true})), false),
        (
            doc(json!({"inputs":{"value":{"type":"unsupported"}},"steps":[]})),
            false,
        ),
        (
            doc(json!({"steps":[{"id":"task","title":"Task","evidence":{"required":["video"]}}]})),
            false,
        ),
        (
            doc(
                json!({"steps":[{"id":"task","title":"Task","evidence":{"required":["file"],"accepted":["screenshot","file"]}}]}),
            ),
            true,
        ),
        (
            doc(json!({"steps":[{"id":"task","title":"Task","actor":"robot"}]})),
            false,
        ),
    ]
}
