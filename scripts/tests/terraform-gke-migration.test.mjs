import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const tfDir = join(dirname(fileURLToPath(import.meta.url)), '../../deploy/terraform/gcp-gke');

test('the GKE module migrates in a Job before the serving deployment rolls', () => {
  assert.ok(existsSync(join(tfDir, 'migration.tf')), 'migration.tf exists');
  const migration = readFileSync(join(tfDir, 'migration.tf'), 'utf8');
  const workload = readFileSync(join(tfDir, 'workload.tf'), 'utf8');

  assert.match(migration, /resource "kubernetes_manifest" "migration_job"/);
  assert.match(migration, /kind\s*=\s*"Job"/);
  assert.match(migration, /image\s*=\s*var\.image/);
  assert.match(migration, /args\s*=\s*\["migrate"\]/);
  assert.match(migration, /name\s*=\s*"cloud-sql-proxy"[\s\S]*?restartPolicy\s*=\s*"Always"/);
  assert.match(migration, /"status\.succeeded"\s*=\s*"1"/);
  assert.match(migration, /DATABASE_URL/);
  assert.match(workload, /RUNBOOK_MIGRATE_ON_START\s*=\s*"false"/);
  assert.match(workload, /depends_on = \[kubernetes_manifest\.migration_job\]/);
});
