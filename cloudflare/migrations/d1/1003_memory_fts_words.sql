-- D1-only (CF3): index memory entries' words, not their JSON.
-- 0026 indexed `body`, which holds the entry as JSON: escapes such as `\n`
-- glue a letter onto the next word ("\nline" tokenizes as "nline"), so a
-- word in the text could be missing from the index. `search_tsv` holds the
-- entry's words (`text_words` of title and body, space-separated), which
-- is what search matches, so the index covers it instead.
DROP TRIGGER IF EXISTS memory_entry_ai;
DROP TRIGGER IF EXISTS memory_entry_ad;
DROP TRIGGER IF EXISTS memory_entry_au;
DROP TABLE IF EXISTS memory_entry_fts;

CREATE VIRTUAL TABLE IF NOT EXISTS memory_entry_fts USING fts5(
    memory_id UNINDEXED,
    search_tsv,
    content='memory_entry',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS memory_entry_ai AFTER INSERT ON memory_entry BEGIN
    INSERT INTO memory_entry_fts (rowid, memory_id, search_tsv)
    VALUES (new.rowid, new.memory_id, new.search_tsv);
END;

CREATE TRIGGER IF NOT EXISTS memory_entry_ad AFTER DELETE ON memory_entry BEGIN
    INSERT INTO memory_entry_fts (memory_entry_fts, rowid, memory_id, search_tsv)
    VALUES ('delete', old.rowid, old.memory_id, old.search_tsv);
END;

CREATE TRIGGER IF NOT EXISTS memory_entry_au AFTER UPDATE ON memory_entry BEGIN
    INSERT INTO memory_entry_fts (memory_entry_fts, rowid, memory_id, search_tsv)
    VALUES ('delete', old.rowid, old.memory_id, old.search_tsv);
    INSERT INTO memory_entry_fts (rowid, memory_id, search_tsv)
    VALUES (new.rowid, new.memory_id, new.search_tsv);
END;

INSERT INTO memory_entry_fts (memory_entry_fts) VALUES ('rebuild');
