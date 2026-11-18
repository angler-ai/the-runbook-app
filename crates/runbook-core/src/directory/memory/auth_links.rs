//! AUTH2 links and invitations for the reference directory: each call is one
//! staged transaction over the whole database, like `commit`.
use super::*;
use crate::auth_links::{
    AuthLink, AuthLinkStore, InvitationRecord, InvitationState, LinkPlan, LinkRequest, LinkState,
    plan_link, sort_invitations, validate_invitation,
};
use runbook_model::InvitationId;

impl AuthLinkStore for MemoryDirectory {
    async fn auth_link(&self, auth_user_id: &str) -> Result<Option<AuthLink>, DirectoryError> {
        Ok(self.database.borrow().auth_links.get(auth_user_id).cloned())
    }
    async fn auth_users_of(&self, user_id: &UserId) -> Result<Vec<String>, DirectoryError> {
        let mut out: Vec<String> = self
            .database
            .borrow()
            .auth_links
            .values()
            .filter(|l| &l.user_id == user_id)
            .map(|l| l.auth_user_id.clone())
            .collect();
        out.sort();
        Ok(out)
    }
    async fn invitations_for_email(
        &self,
        email: &str,
        now_ms: i64,
    ) -> Result<Vec<InvitationRecord>, DirectoryError> {
        let key = email_key(email)?;
        let mut out: Vec<_> = self
            .database
            .borrow()
            .invitations
            .values()
            .filter(|i| {
                i.email == key && i.state == InvitationState::Pending && i.expires_at_ms > now_ms
            })
            .cloned()
            .collect();
        sort_invitations(&mut out);
        Ok(out)
    }
    async fn link_identity(
        &self,
        org: &OrganizationId,
        request: LinkRequest,
    ) -> Result<AuthLink, DirectoryError> {
        let mut db = self.database.borrow_mut();
        let mut staged = db.clone();
        let snapshot = staged.orgs.get(org).ok_or(DirectoryError::NotFound)?;
        let revision = snapshot.revision;
        let email = email_key(&request.verified_email)?;
        let plan = plan_link(
            org,
            &request,
            LinkState {
                existing: staged.auth_links.get(&request.auth_user_id),
                invitation: request
                    .invitation
                    .as_ref()
                    .and_then(|id| staged.invitations.get(id)),
                email_owner: staged.emails.get(&email),
                snapshot,
                identity_name: staged.users.get(&request.user_id).map(|u| u.name.as_str()),
            },
        )?;
        let (mutations, link, accept) = match plan {
            LinkPlan::Existing(link) => return Ok(link),
            LinkPlan::Commit {
                mutations,
                link,
                accept,
            } => (mutations, link, accept),
        };
        if staged.apply(DirectoryCommit {
            org_id: org.clone(),
            expected_revision: revision,
            mutations,
        })? == CommitResult::Conflict
        {
            return Err(DirectoryError::Backend("memory.conflict"));
        }
        if let Some(id) = accept {
            let invitation = staged
                .invitations
                .get_mut(&id)
                .ok_or(DirectoryError::Invalid("invitation"))?;
            invitation.state = InvitationState::Accepted;
            invitation.accepted_by = Some(request.user_id.clone());
            invitation.closed_at_ms = Some(request.now_ms);
        }
        staged
            .auth_links
            .insert(link.auth_user_id.clone(), link.clone());
        *db = staged;
        Ok(link)
    }
    async fn create_invitation(
        &self,
        org: &OrganizationId,
        invitation: InvitationRecord,
    ) -> Result<(), DirectoryError> {
        let mut db = self.database.borrow_mut();
        let snapshot = db.orgs.get(org).ok_or(DirectoryError::NotFound)?;
        let invitation = validate_invitation(org, &invitation, snapshot)?;
        if db.invitations.contains_key(&invitation.id)
            || db.invitations.values().any(|i| {
                &i.org_id == org
                    && i.email == invitation.email
                    && i.state == InvitationState::Pending
                    && i.expires_at_ms > invitation.created_at_ms
            })
        {
            return Err(DirectoryError::Duplicate);
        }
        db.invitations.insert(invitation.id.clone(), invitation);
        Ok(())
    }
    async fn list_invitations(
        &self,
        org: &OrganizationId,
    ) -> Result<Vec<InvitationRecord>, DirectoryError> {
        let mut out: Vec<_> = self
            .database
            .borrow()
            .invitations
            .values()
            .filter(|i| &i.org_id == org)
            .cloned()
            .collect();
        sort_invitations(&mut out);
        Ok(out)
    }
    async fn revoke_invitation(
        &self,
        org: &OrganizationId,
        id: &InvitationId,
        now_ms: i64,
    ) -> Result<bool, DirectoryError> {
        let mut db = self.database.borrow_mut();
        let Some(invitation) = db
            .invitations
            .get_mut(id)
            .filter(|i| &i.org_id == org && i.state == InvitationState::Pending)
        else {
            return Ok(false);
        };
        invitation.state = InvitationState::Revoked;
        invitation.closed_at_ms = Some(now_ms);
        Ok(true)
    }
}
