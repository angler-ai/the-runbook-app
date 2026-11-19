//! Schedule passes: archived projects' schedules are off, and a failed
//! directory read fails closed.
#[path = "support/fake_backends.rs"]
mod fake_backends;
use fake_backends::{Fake, run};
use runbook_api::triggers::{SchedulePass, due_schedules};
use runbook_core::{catalog::TenantScope, triggers::PublishedVersion};

#[test]
fn an_unreadable_directory_skips_the_organizations_schedules() {
    let fake = Fake::new();
    // No such organization in the directory: its snapshot read fails.
    fake.published.borrow_mut().push(PublishedVersion {
        scope: TenantScope {
            org_id: format!("org_{:026}", 77).parse().unwrap(),
            project_id: format!("prj_{:026}", 77).parse().unwrap(),
        },
        runbook_id: format!("rb_{:026}", 77).parse().unwrap(),
        version_id: format!("rbv_{:026}", 77).parse().unwrap(),
        number: 1.try_into().unwrap(),
        snapshot_digest: [7; 32],
    });
    let mut pass = SchedulePass::default();
    let due = run(due_schedules(&fake, 1_000, &mut pass)).unwrap();
    assert!(due.is_empty());
    assert_eq!(pass.scheduled, 0);
    assert_eq!(
        pass.failed,
        [(format!("rb_{:026}", 77), "storage.unavailable")]
    );
}
