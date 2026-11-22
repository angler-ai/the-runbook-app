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

test('migration Job: its proxy reaches a private-IP instance and the kubelet can probe it', async () => {
  const { readFileSync } = await import('node:fs');
  const migration = readFileSync(`${tfDir}/migration.tf`, 'utf8');
  // Like the Deployment's proxy: private IP, health server on the pod IP.
  assert.match(migration, /"--private-ip"/);
  assert.match(migration, /"--http-address=0\.0\.0\.0"/);
  // Postgres listens on 127.0.0.1 only, so a tcpSocket probe on 5432 never passes.
  assert.doesNotMatch(migration, /tcpSocket/);
  assert.match(migration, /httpGet\s*=\s*\{\s*path\s*=\s*"\/startup"\s*port\s*=\s*9801/);
  // The Service and PDB select app.kubernetes.io/name = runbook; the Job's pod must not match.
  const podLabels = migration.match(/template = \{\s*#[^\n]*\n[^\n]*\n\s*metadata = \{\s*labels = \{([^}]*)\}/s);
  assert.ok(podLabels, 'pod template labels found');
  assert.doesNotMatch(podLabels[1], /"app\.kubernetes\.io\/name"\s*=\s*"runbook"\s*$/m);
});

test('every container sets ephemeral-storage, so Autopilot does not change the applied manifest', async () => {
  const { readFileSync } = await import('node:fs');
  const migration = readFileSync(`${tfDir}/migration.tf`, 'utf8');
  const workload = readFileSync(`${tfDir}/workload.tf`, 'utf8');
  // Two containers each: requests and limits.
  assert.equal((migration.match(/"ephemeral-storage"\s*=\s*"1Gi"/g) || []).length, 4);
  assert.equal((workload.match(/"ephemeral-storage"\s*=\s*"1Gi"/g) || []).length, 4);
});

test('the load balancer is a trusted proxy, and config or secret changes roll the pods', async () => {
  const { readFileSync } = await import('node:fs');
  const workload = readFileSync(`${tfDir}/workload.tf`, 'utf8');
  assert.match(
    workload,
    /RUNBOOK_TRUSTED_PROXIES\s*=\s*"35\.191\.0\.0\/16,130\.211\.0\.0\/16,\$\{google_compute_global_address\.ingress_ip\.address\}"/,
  );
  assert.match(workload, /"runbook\.dev\/config-sha256"\s*=\s*sha256\(jsonencode\(kubernetes_config_map\.runbook_config\.data\)\)/);
  // The managed Secret is optional (manage_secret_values); when the module manages it, its hash rolls the pods.
  assert.match(workload, /var\.manage_secret_values \? \{\s*"runbook\.dev\/secret-sha256"\s*=\s*nonsensitive\(sha256\(jsonencode\(kubernetes_secret\.runbook_secrets\[0\]\.data\)\)\)/);
});

test('the app container runs as the image user by number, and the image names it by number', async () => {
  const { readFileSync } = await import('node:fs');
  const workload = readFileSync(`${tfDir}/workload.tf`, 'utf8');
  assert.match(workload, /run_as_non_root\s*=\s*true\s*run_as_user\s*=\s*10001\s*run_as_group\s*=\s*10001/);
  const dockerfile = readFileSync(new URL('../../Dockerfile', import.meta.url), 'utf8');
  assert.match(dockerfile, /^USER 10001:10001$/m);
  assert.match(dockerfile, /useradd --system --uid 10001/);
});

test('the Service ignores the NEG status annotation GKE writes onto it', async () => {
  const { readFileSync } = await import('node:fs');
  const workload = readFileSync(`${tfDir}/workload.tf`, 'utf8');
  assert.match(workload, /ignore_changes\s*=\s*\[metadata\[0\]\.annotations\["cloud\.google\.com\/neg-status"\]\]/);
});
