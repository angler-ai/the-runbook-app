// Test that deploy/terraform/gcp-gke formatting and syntax validate cleanly.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';

const tfDir = new URL('../../deploy/terraform/gcp-gke', import.meta.url).pathname;

function findTerraform() {
  if (existsSync('/opt/homebrew/bin/terraform')) return '/opt/homebrew/bin/terraform';
  const probe = spawnSync('which', ['terraform'], { encoding: 'utf8' });
  if (probe.status === 0 && probe.stdout.trim()) return probe.stdout.trim();
  return null;
}

test('terraform fmt and validate for deploy/terraform/gcp-gke', (t) => {
  const terraform = findTerraform();
  if (!terraform) {
    t.skip('terraform binary not found');
    return;
  }

  // Check formatting
  const fmt = spawnSync(terraform, ['fmt', '-check', '-recursive'], {
    cwd: tfDir,
    encoding: 'utf8',
  });
  assert.equal(fmt.status, 0, `terraform fmt -check failed:\n${fmt.stdout}\n${fmt.stderr}`);

  // Validate configuration
  const init = spawnSync(terraform, ['init', '-backend=false'], {
    cwd: tfDir,
    encoding: 'utf8',
  });
  assert.equal(init.status, 0, `terraform init -backend=false failed:\n${init.stdout}\n${init.stderr}`);

  const val = spawnSync(terraform, ['validate'], {
    cwd: tfDir,
    encoding: 'utf8',
  });
  assert.equal(val.status, 0, `terraform validate failed:\n${val.stdout}\n${val.stderr}`);
});

test('Google sign-in: the ID goes to the ConfigMap, the secret only to the Secret', async () => {
  const { readFileSync } = await import('node:fs');
  const read = (name) => readFileSync(`${tfDir}/${name}`, 'utf8');
  const variables = read('variables.tf');
  assert.match(variables, /variable "google_client_secret" \{[^}]*sensitive\s*=\s*true/s);
  assert.match(read('workload.tf'), /RUNBOOK_GOOGLE_CLIENT_ID\s*=\s*var\.google_client_id/);
  assert.doesNotMatch(read('workload.tf'), /google_client_secret/);
  assert.match(read('secrets.tf'), /RUNBOOK_GOOGLE_CLIENT_SECRET\s*=\s*var\.google_client_secret/);
  assert.match(read('ingress.tf'), /sslPolicy\s*=\s*var\.ssl_policy/);
});
