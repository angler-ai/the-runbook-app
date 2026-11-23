ALTER TABLE credential_oauth
  ADD COLUMN refresh_failures integer NOT NULL DEFAULT 0,
  ADD COLUMN forced_refreshes integer NOT NULL DEFAULT 0,
  ADD COLUMN access_rejected_since timestamptz,
  ADD COLUMN purge_until timestamptz;
