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
