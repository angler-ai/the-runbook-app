-- D1-only (CF3, CF2-9): which execution an execution or refresh token
-- belongs to, by the token's SHA-256. The api Worker writes the row before
-- it forwards a mint or refresh to the ExecutionDO (it holds the hashes
-- before the commit), so it can route a bearer to its object. The row only
-- routes: validity, expiry and revocation are always rechecked in the DO.
CREATE TABLE IF NOT EXISTS execution_token_route (
    hash BLOB PRIMARY KEY NOT NULL CHECK (length(hash) = 32),
    org_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS execution_token_route_execution
    ON execution_token_route (execution_id);
