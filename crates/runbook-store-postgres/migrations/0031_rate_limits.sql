-- HOST1: rate-limit windows every server instance shares, so a limit holds
-- across replicas on one database (device-code requests per client address,
-- device approval misses per user). One row per key: the key is a SHA-256
-- hash (keys name client addresses and users), `hits` the store-clock times
-- (ms) of the hits still inside the window, oldest first. A row is purged
-- once `expires_at_ms` (its newest hit plus the window) has passed.
CREATE TABLE runbook.rate_window (
    key_hash text PRIMARY KEY,
    hits bigint[] NOT NULL,
    expires_at_ms bigint NOT NULL
);
CREATE INDEX rate_window_expires ON runbook.rate_window (expires_at_ms);
