-- INTERIM (pilot only): seed one person's first personal access token.
--
-- The pilot has no login flow that issues a person's first credential (the
-- device grant is deferred; `runbook login` takes a token on stdin), and
-- `POST /v1/tokens` only issues tokens to the caller. Until then the
-- operator inserts one short-scoped seed per person, exactly as the C4
-- flagship gate does; the person then mints their own working token from it
-- with `runbook token create`.
--
-- Only the SHA-256 of the secret reaches the database, never the secret.
-- The seed is a human personal token for one active user, limited to one
-- project, with `executions:read`, `executions:start`, `executions:act`
-- and `tokens:manage`, expiring in 90 days. Nothing is inserted unless the
-- user is active in the organization that owns the project; the statement
-- returns the token ID when it inserted the seed.
--
-- Run with psql variables (examples/pilot/seed-first-token.sh does this):
--   psql -X -q -t -A -v ON_ERROR_STOP=1 \
--     -v token_id=tok_... -v org_id=org_... -v user_id=usr_... \
--     -v project_id=prj_... -v hash_hex=<64 hex chars> < seed-first-token.sql
WITH target AS (
    SELECT u.org_id, u.user_id, p.project_id,
           decode(:'hash_hex', 'hex') AS hash,
           floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint AS now_ms
    FROM runbook.org_user u
    JOIN runbook.project p ON p.org_id = u.org_id
    WHERE u.org_id = :'org_id'
      AND u.user_id = :'user_id'
      AND u.active
      AND p.project_id = :'project_id'
      AND octet_length(decode(:'hash_hex', 'hex')) = 32
)
INSERT INTO runbook.credential (id, org_id, secret_hash, payload)
SELECT :'token_id', t.org_id, t.hash,
       json_build_object(
           'id', :'token_id',
           'kind', 'personal',
           'hash', (SELECT json_agg(get_byte(t.hash, i) ORDER BY i)
                    FROM generate_series(0, 31) AS i),
           'principal', json_build_object(
               'User', json_build_object('user_id', t.user_id, 'actor', 'human')),
           'org_id', t.org_id,
           'scopes', json_build_array(
               'runbooks:read', 'executions:read', 'executions:start', 'executions:act',
               'tokens:manage'),
           'projects', json_build_array(t.project_id),
           'expires_at', t.now_ms + 90::bigint * 86400000,
           'revoked_at', NULL,
           'last_used_at', NULL,
           'management', json_build_object(
               'name', 'first token (operator seed)',
               'created_at', t.now_ms,
               'created_by', NULL)
       )::text
FROM target t
RETURNING id;
