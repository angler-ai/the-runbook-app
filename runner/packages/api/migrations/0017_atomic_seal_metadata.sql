-- The vault commits display metadata alongside the encrypted version and audit.
GRANT UPDATE (secret_meta, status, updated_at) ON credentials TO rb_vault;

-- Recover display metadata left behind by a pre-migration lost response.
WITH latest AS (
  SELECT DISTINCT ON (credential_id, field) credential_id, field, version, fingerprint,
    binding_hash, created_at, created_by, destroyed_at
  FROM credential_secret_versions
  ORDER BY credential_id, field, version DESC
), recovered AS (
  SELECT c.id, jsonb_object_agg(l.field, jsonb_build_object(
    'version', l.version, 'fingerprint', l.fingerprint, 'created_at', l.created_at,
    'created_by', l.created_by, 'sealed_for_binding',
    COALESCE(l.binding_hash = c.binding_hash AND l.destroyed_at IS NULL, false)
  )) AS meta, bool_or(l.binding_hash = c.binding_hash AND l.destroyed_at IS NULL AND
    l.field = CASE c.type WHEN 'api-key' THEN 'key' WHEN 'model-provider' THEN 'api_key' WHEN 'mcp-bearer' THEN 'token' END) AS ready
  FROM credentials c JOIN latest l ON l.credential_id = c.id
  WHERE c.status <> 'revoked' AND c.deleted_at IS NULL
  GROUP BY c.id
)
UPDATE credentials c SET secret_meta = recovered.meta,
  status = CASE WHEN recovered.ready THEN 'active' ELSE 'needs_secret' END
FROM recovered WHERE recovered.id = c.id;
