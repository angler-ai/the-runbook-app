-- D1: 1018_runbook_listing.sql (UI-1b)
-- The runbook list reads rows only: each runbook keeps its current draft's
-- list fields beside the draft, written with the draft
-- (crates/runbook-store-d1/src/listing.rs), so a list request never reads a
-- source from R2 (CF11 keeps every source there).
--   listing          JSON {title, description (at most 300 characters),
--                    description_truncated, tags}: what the list shows.
--   listing_search   BLOB: the lowercased slug, title, description and
--                    tags (each its first 2,000 characters), separated by
--                    the byte 0xFF (never in UTF-8): what `q` matches with
--                    BLOB instr (bytes, so a NUL is an ordinary byte).
--   listing_digest   SHA-256 of the draft's source (`draft_ahead` compares
--                    it with the latest version's source_digest).
--   listing_revision the draft_revision the listing describes.
--   listing_format   runbook_core::listing::LISTING_FORMAT when computed.
-- A row whose listing_revision differs from draft_revision, or whose
-- listing_format differs from the current one, is recomputed by the next
-- list request: every row written before this migration (all NULL), and a
-- draft saved by an older release after a rollback (it bumps
-- draft_revision and leaves these alone). SQL cannot parse a runbook or
-- read R2, so this migration adds the columns only; a list request derives
-- at most 100 such rows (UI-1b review round 1), and a page that stops there
-- continues from the last row it examined. A listing larger than 64 KiB is
-- not stored (listing NULL), so a draft write never meets D1's row limit.
ALTER TABLE runbook ADD COLUMN listing TEXT;
ALTER TABLE runbook ADD COLUMN listing_search BLOB;
ALTER TABLE runbook ADD COLUMN listing_digest BLOB;
ALTER TABLE runbook ADD COLUMN listing_revision INTEGER;
ALTER TABLE runbook ADD COLUMN listing_format INTEGER;
