CREATE TABLE environments (
  id text PRIMARY KEY,
  org_id text NOT NULL,
  name text NOT NULL,
  description text NOT NULL DEFAULT '',
  is_default boolean NOT NULL DEFAULT false,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  deleted_at timestamptz
);
CREATE UNIQUE INDEX environments_org_name ON environments (org_id, lower(name)) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX environments_org_default ON environments (org_id) WHERE is_default;

CREATE TABLE environment_versions (
  id text PRIMARY KEY,
  environment_id text NOT NULL REFERENCES environments (id),
  version integer NOT NULL CHECK (version > 0),
  config jsonb NOT NULL,
  image_digest text,
  UNIQUE (environment_id, version)
);

CREATE TABLE environment_registries (
  org_id text PRIMARY KEY,
  registries jsonb NOT NULL CHECK (jsonb_typeof(registries) = 'array')
);

GRANT SELECT, INSERT, UPDATE ON environments, environment_registries TO rb_api;
GRANT SELECT, INSERT ON environment_versions TO rb_api;
GRANT SELECT ON environments, environment_versions TO rb_scheduler;

CREATE TABLE environment_installation_images (
  org_id text PRIMARY KEY,
  image_digest text NOT NULL
);
GRANT SELECT, INSERT, UPDATE ON environment_installation_images TO rb_api;
