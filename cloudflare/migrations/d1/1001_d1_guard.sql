-- D1-only (CF3): batch guards. D1 has no interactive transactions, so a
-- write that depends on what it read runs as one batch that starts with
-- guard statements: `INSERT INTO d1_guard (code) SELECT ? WHERE NOT (...)`.
-- A failed condition inserts a row, and this trigger aborts the whole batch
-- with `rb.guard:<code>`, so nothing in it commits. No row is ever stored.
CREATE TABLE IF NOT EXISTS d1_guard (
    code TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS d1_guard_abort BEFORE INSERT ON d1_guard BEGIN
    SELECT RAISE(ABORT, 'rb.guard:' || NEW.code);
END;
