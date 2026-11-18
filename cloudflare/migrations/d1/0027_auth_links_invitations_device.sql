-- D1: 0027_auth_links_invitations_device.sql
-- Auth links, organization invitations, and RFC 8628 device codes.

CREATE TABLE IF NOT EXISTS auth_link (
    auth_user_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organization (org_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    email TEXT NOT NULL,
    linked_at INTEGER NOT NULL,
    FOREIGN KEY (org_id, user_id) REFERENCES org_user (org_id, user_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS auth_link_user ON auth_link (org_id, user_id);

CREATE TABLE IF NOT EXISTS invitation (
    invitation_id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organization (org_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    projects TEXT NOT NULL,
    invited_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > created_at),
    state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'revoked')),
    accepted_by TEXT,
    closed_at INTEGER
);
CREATE INDEX IF NOT EXISTS invitation_org ON invitation (org_id, created_at DESC, invitation_id DESC);
CREATE INDEX IF NOT EXISTS invitation_pending_email ON invitation (email) WHERE state = 'pending';

CREATE TABLE IF NOT EXISTS device_code (
    device_code_hash BLOB PRIMARY KEY NOT NULL CHECK (length(device_code_hash) = 32),
    user_code_hash BLOB NOT NULL UNIQUE CHECK (length(user_code_hash) = 32),
    org_id TEXT,
    expires_at INTEGER NOT NULL,
    payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS device_code_expiry ON device_code (expires_at);
