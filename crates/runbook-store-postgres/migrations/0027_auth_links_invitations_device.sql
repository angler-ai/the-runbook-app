-- AUTH2 (D45, D47): sign-in identity links, invitations and device codes.
-- Better Auth's own tables live in the separate `auth` schema, which Rust
-- never touches; these are directory facts the Rust side owns.

-- One Better Auth account to one directory user (D45), written in the same
-- transaction as the directory commit that binds the verified email.
CREATE TABLE runbook.auth_link (
    auth_user_id text PRIMARY KEY,
    org_id text NOT NULL REFERENCES runbook.organization ON DELETE CASCADE,
    user_id text NOT NULL,
    email text NOT NULL,
    linked_at bigint NOT NULL,
    FOREIGN KEY (org_id, user_id) REFERENCES runbook.org_user ON DELETE CASCADE
);
CREATE INDEX auth_link_user ON runbook.auth_link (org_id, user_id);

-- Emails are normalized by the adapter (directory::email_key). `projects` is
-- the JSON list of [project_id, role] pairs applied on acceptance.
CREATE TABLE runbook.invitation (
    invitation_id text PRIMARY KEY,
    org_id text NOT NULL REFERENCES runbook.organization ON DELETE CASCADE,
    email text NOT NULL,
    role text NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    projects text NOT NULL,
    invited_by text NOT NULL,
    created_at bigint NOT NULL,
    expires_at bigint NOT NULL CHECK (expires_at > created_at),
    state text NOT NULL CHECK (state IN ('pending', 'accepted', 'revoked')),
    accepted_by text,
    closed_at bigint
);
CREATE INDEX invitation_org ON runbook.invitation (org_id, created_at DESC, invitation_id DESC);
CREATE INDEX invitation_pending_email ON runbook.invitation (email) WHERE state = 'pending';

-- RFC 8628 device codes (D47). Only SHA-256 hashes of the device and user
-- codes are stored; `payload` is the typed record. A pending code belongs to
-- no organization yet; `org_id` is set when it is approved.
CREATE TABLE runbook.device_code (
    device_code_hash bytea PRIMARY KEY CHECK (octet_length(device_code_hash) = 32),
    user_code_hash bytea NOT NULL UNIQUE CHECK (octet_length(user_code_hash) = 32),
    org_id text,
    expires_at bigint NOT NULL,
    payload text NOT NULL
);
CREATE INDEX device_code_expiry ON runbook.device_code (expires_at);
