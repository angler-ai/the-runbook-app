ALTER TABLE ledger_reservations
  ADD COLUMN capability_hash bytea,
  ADD COLUMN usage_pending boolean NOT NULL DEFAULT false;
