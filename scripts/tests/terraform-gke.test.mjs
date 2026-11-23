// Test that deploy/terraform/gcp-gke formatting and syntax validate cleanly.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const tfDir = fileURLToPath(new URL('../../deploy/terraform/gcp-gke', import.meta.url));

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
  const podLabels = migration.match(/template = \{\s*#[^\n]*\n[^\n]*\n\s*metadata = \{\s*labels = merge\(var\.labels, \{([^}]*)\}/s);
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
    /RUNBOOK_TRUSTED_PROXIES\s*=\s*"35\.191\.0\.0\/16,130\.211\.0\.0\/16,\$\{google_compute_global_address\.ingress_ip\[0\]\.address\}"/,
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

test('the runner switch selects five process KSAs and only the database identities', (t) => {
  const terraform = findTerraform();
  if (!terraform || !existsSync(`${tfDir}/.terraform`)) {
    t.skip('Terraform providers are unavailable');
    return;
  }
  const state = mkdtempSync(join(tmpdir(), 'runbook-runner-terraform-'));
  t.after(() => rmSync(state, { recursive: true, force: true }));
  const names = (enabled) => {
    const result = spawnSync(terraform, ['console', '-no-color', `-state=${join(state, 'terraform.tfstate')}`, '-var', `runner_enabled=${enabled}`,
      '-var', `runner_image=ghcr.io/angler-ai/runbook-runner@sha256:${'a'.repeat(64)}`,
      '-var', `runner_sandbox_image=ghcr.io/angler-ai/runbook-sandbox@sha256:${'b'.repeat(64)}`,
    ], {
      cwd: tfDir, encoding: 'utf8', input: 'jsonencode([sort(tolist(local.runner_processes)), sort(tolist(local.runner_google_identities))])\n',
    });
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(JSON.parse(result.stdout));
  };
  assert.deepEqual(names(false), [[], []]);
  assert.deepEqual(names(true), [['api', 'gateway', 'sandbox-ctl', 'vault', 'worker'], ['api', 'bootstrap', 'vault']]);
});

test('sandbox permissions, admission, and gateway listener range are restricted', async () => {
  const { readFileSync } = await import('node:fs');
  const read = (name) => readFileSync(`${tfDir}/${name}`, 'utf8');
  const sandbox = read('runner-sandboxes.tf');
  assert.match(sandbox, /resources\s*=\s*\["pods\/exec"\]\s*verbs\s*=\s*\["get", "create"\]/);
  assert.match(sandbox, /failurePolicy\s*=\s*"Fail"/);
  assert.match(sandbox, /paramKind\s*=\s*\{ apiVersion = "v1", kind = "ConfigMap" \}/);
  assert.match(sandbox, /parameterNotFoundAction\s*=\s*"Deny"/);
  assert.match(sandbox, /params\.data\.exists\(k, params\.data\[k\] == c\.image\)/);
  assert.match(sandbox, /validationActions\s*=\s*\["Deny"\]/);
  assert.match(sandbox, /resource "kubernetes_resource_quota_v1"/);
  assert.match(sandbox, /resource "kubernetes_limit_range_v1"/);
  assert.match(sandbox, /object\.spec\.runtimeClassName == 'gvisor'/);
  const network = read('runner-network.tf');
  assert.equal([...network.matchAll(/port\s*=\s*"20000"\s+end_port\s*=\s*29999/g)].length, 2);
  assert.match(network, /"199\.36\.153\.4\/30"\s*=\s*443/);
  assert.match(network, /"169\.254\.0\.0\/16"/);
});

test('runner workloads mount CSI references and the bootstrap job uses private-IP IAM authentication', async () => {
  const { readFileSync } = await import('node:fs');
  const read = (name) => readFileSync(`${tfDir}/${name}`, 'utf8');
  const workloads = read('runner-workloads.tf');
  assert.match(workloads, /driver\s*=\s*"secrets-store-gke\.csi\.k8s\.io"/);
  assert.doesNotMatch(workloads, /secret_data|kubernetes_secret"/);
  assert.match(workloads, /"--private-ip", "--auto-iam-authn"/);
  assert.match(workloads, /strategy\s*=\s*\{ type = "Recreate" \}/);
  assert.match(read('runner-database.tf'), /merge\(local\.runner_proxy, \{ restartPolicy = "Always" \}\)/);
  assert.match(read('runner-database.tf'),
    /"runner_database_migration" \{[\s\S]*?computed_fields\s*=\s*\[[^\]]*"spec.template.metadata.labels"/,
    'the Kubernetes Job controller adds labels to the submitted pod template');
  assert.match(read('workload.tf'), /RUNBOOK_RUNNER_PRIVATE_URL\s*=\s*local\.runner_api_url/);
});

test('OAuth callback backend disables access logging', async () => {
  const { readFileSync } = await import('node:fs');
  assert.match(readFileSync(`${tfDir}/ingress.tf`, 'utf8'), /logging\s*=\s*\{\s*enable\s*=\s*false/);
});

test('sandbox policy permits only the authenticated gateway listener range', async () => {
  const { readFileSync } = await import('node:fs');
  const policy = readFileSync(`${tfDir}/runner-network.tf`, 'utf8').split('"sandboxes" {')[1].split('\nresource ')[0];
  assert.equal([...policy.matchAll(/\begress \{/g)].length, 1);
  assert.match(policy, /end_port = 29999/);
});

test('enabling the runner asserts existing cluster capabilities and denies unlabeled pods by default', async () => {
  const { readFileSync } = await import('node:fs');
  const setup = readFileSync(`${tfDir}/runner-setup.tf`, 'utf8');
  for (const check of [/precondition/, /enable_autopilot/, /ADVANCED_DATAPATH/, /runner_supports_end_port/, /gvisor/]) {
    assert.match(setup, check);
  }
  assert.match(readFileSync(`${tfDir}/runner-network.tf`, 'utf8'),
    /"runner_default_deny" \{[\s\S]*?pod_selector \{\}\s+policy_types = \["Ingress", "Egress"\]/);
});

test('infrastructure-only stage has no secret IAM instances for absent references', async () => {
  const { readFileSync } = await import('node:fs');
  const workloads = readFileSync(`${tfDir}/runner-workloads.tf`, 'utf8');
  assert.match(workloads, /"google_secret_manager_secret_iam_member" "runner" \{\s*for_each\s*= var.runner_enabled \? var.runner_secret_versions : \{\}\s*secret_id\s*= split\("\/versions\/", each.value\)\[0\]/);
});

test('sandbox IAM setup rejects conditional project, ancestor and resource grants', (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'runbook-sandbox-iam-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return import('node:fs').then(({ writeFileSync }) => {
    writeFileSync(join(dir, 'gcloud'), `#!/bin/sh
case "$1 $2" in
  "projects get-ancestors") echo '[{"type":"project","id":"123"},{"type":"folder","id":"456"},{"type":"organization","id":"789"}]' ;;
  "projects get-iam-policy"|"resource-manager folders"|"organizations get-iam-policy")
    if [ "$1" = "$RB_SCOPE" ]; then printf '%s' "$RB_POLICY"; else echo '{"bindings":[]}'; fi ;;
  "asset search-all-iam-policies")
    [ "$3" = "--scope=organizations/789" ] || exit 3
    [ "$4" = '--query=policy:"example.svc.id.goog" OR policy:allAuthenticatedUsers' ] || exit 4
    if [ "$1" = "$RB_SCOPE" ]; then printf '[{"policy":%s}]' "$RB_POLICY"; else echo '[]'; fi ;;
  *) exit 2 ;;
esac
`, { mode: 0o700 });
    const pool = 'iam.googleapis.com/projects/123/locations/global/workloadIdentityPools/example.svc.id.goog';
    const run = (scope, member) => spawnSync('python3', [join(tfDir, 'check-sandbox-iam.py')], {
      input: JSON.stringify({ project: 'example', project_number: '123', cluster_name: 'example', location: 'us-east1', sandbox_uid: 'sandbox-uid',
        sandbox_namespace: 'runbook-test2-runbook-sandboxes', sandbox_account: 'runbook-test2-sandbox' }),
      env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, RB_SCOPE: scope,
        RB_POLICY: JSON.stringify({ bindings: [{ role: 'roles/viewer', members: [member],
          condition: { expression: 'true' } }] }) },
      encoding: 'utf8',
    });
    const allowed = run('asset', `principal://${pool}/subject/ns/runbook-runner/sa/vault`);
    assert.equal(allowed.status, 0, allowed.stderr);
    for (const [scope, member] of [
      ['projects', 'allAuthenticatedUsers'],
      ['asset', 'allAuthenticatedUsers'],
      ['projects', `principalSet://${pool}/*`],
      ['resource-manager', `principalSet://${pool}/namespace/runbook-test2-runbook-sandboxes`],
      ['organizations', 'serviceAccount:example.svc.id.goog[runbook-test2-runbook-sandboxes/runbook-test2-sandbox]'],
      ['asset', `principal://${pool}/subject/ns/runbook-test2-runbook-sandboxes/sa/runbook-test2-sandbox`],
      ['asset', `principal://${pool}/kubernetes.serviceaccount.uid/sandbox-uid`],
      ['asset', `principalSet://${pool}/kubernetes.cluster/https://container.googleapis.com/v1/projects/example/locations/us-east1/clusters/example`],
    ]) {
      const denied = run(scope, member);
      assert.notEqual(denied.status, 0);
      assert.match(denied.stderr, /grants access to the sandbox/);
    }
  });
});

const planInputs = `
variables {
  project_id = "runbook-plan-fixture"
  region = "us-central1"
  network = "projects/runbook-plan-fixture/global/networks/fixture"
  subnetwork = "projects/runbook-plan-fixture/regions/us-central1/subnetworks/fixture"
  create_cluster = true
  app_domain = "runbook.example"
  image = "example.com/runbook@sha256:${'a'.repeat(64)}"
  runner_image = "ghcr.io/angler-ai/runbook-runner@sha256:${'b'.repeat(64)}"
  runner_sandbox_image = "ghcr.io/angler-ai/runbook-sandbox@sha256:${'c'.repeat(64)}"
  cloud_sql_private_cidr = "10.42.0.0/24"
  blob_store = "gcs"
}

mock_provider "google" {}
override_resource {
  target = google_container_cluster.autopilot[0]
  values = {
    endpoint = "192.0.2.1"
    master_auth = [{ cluster_ca_certificate = "Y2E=" }]
    master_version = "1.35.0-gke.1"
  }
}
mock_provider "google-beta" {}
mock_provider "kubernetes" {}
mock_provider "random" {}
mock_provider "external" {}
override_data {
  target = data.google_project.runner[0]
  values = { number = "123456789" }
}
override_data {
  target = data.external.runner_sandbox_iam[0]
  values = { result = { safe = "true" } }
}
`;

function planFixture(t, terraform, config, baseline = false) {
  const dir = mkdtempSync(join(tmpdir(), 'runbook-gke-plan-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const run = (args, options) => {
    const result = spawnSync(args[0], args.slice(1), {
      cwd: dir, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, ...options,
    });
    assert.equal(result.status, 0, `${args.join(' ')} failed:\n${result.stderr}\n${result.stdout.slice(-8000)}`);
    return result.stdout;
  };
  const configFiles = (name) => name.endsWith('.tf') || name === '.terraform.lock.hcl';
  const files = baseline
    ? run(['git', '-C', tfDir, 'ls-tree', '--name-only', '--full-tree', '78615e9f:deploy/terraform/gcp-gke']).trim().split('\n').filter(configFiles)
    : readdirSync(tfDir).filter(configFiles);
  for (const name of files) {
    const content = baseline
      ? run(['git', '-C', tfDir, 'show', `78615e9f:deploy/terraform/gcp-gke/${name}`])
      : readFileSync(join(tfDir, name), 'utf8');
    // The Kubernetes mock does not use transport settings. Terraform 1.9
    // otherwise tries to index the mock cluster's unpopulated CA list.
    writeFileSync(join(dir, name), name === 'main.tf'
      ? content.replace(/^  cluster_ca_cert\s*=.*$/m, '  cluster_ca_cert = ""')
      : content);
  }
  const providers = join(tfDir, '.terraform', 'providers');
  const init = [terraform, 'init', '-backend=false', '-input=false', '-lockfile=readonly', '-no-color'];
  if (existsSync(providers)) init.push(`-plugin-dir=${providers}`);
  run(init);
  mkdirSync(join(dir, 'tests'));
  writeFileSync(join(dir, 'tests', 'deployment.tftest.hcl'), planInputs + config);
  const result = run([terraform, 'test', '-json', '-verbose']);
  return result.trim().split('\n').map((line) => JSON.parse(line))
    .filter((entry) => entry.type === 'test_plan')
    .map((entry) => entry.test_plan);
}

const enrolledRunnerInputs = `
    runner_database_bootstrap_ready = true
    runner_install = {
      id = "fixture"
      org_id = "fixture"
      group_id = "fixture"
      runner_id = "fixture"
      role_prefix = "fixture"
    }
    runner_secret_versions = {
${['api', 'gateway', 'sandbox-ctl', 'vault', 'worker'].map((role) =>
    `      ${role} = "projects/runbook-plan-fixture/secrets/runbook-test-${role}/versions/1"`).join('\n')}
    }
`;

test('default GKE plans retain the resource arguments from 78615e9f', (t) => {
  const terraform = findTerraform();
  if (!terraform) return t.skip('terraform binary not found');

  const runs = `
run "core" {
  command = plan
}
run "runner" {
  command = plan
  variables {
    runner_enabled = true
    database_auth = "iam"
    ${enrolledRunnerInputs}
  }
}
`;
  const before = planFixture(t, terraform, runs, true);
  const after = planFixture(t, terraform, runs);
  assert.equal(before.length, 2);
  assert.equal(after.length, 2);
  for (const [index, oldPlan] of before.entries()) {
    // Adding count to the ingress creates an automatic state move. Compare
    // the resource arguments independent of the optional singleton index.
    const argumentsByAddress = (plan) => Object.fromEntries(plan.resource_changes
      .filter((resource) => resource.mode === 'managed')
      .map(({ address, change }) => [address.replace(/^(google_compute_global_address\.ingress_ip|kubernetes_manifest\.(?:backend_config|frontend_config|managed_certificate)|kubernetes_ingress_v1\.runbook_ingress)\[0\]$/, '$1'), change.after]));
    const expected = argumentsByAddress(oldPlan);
    // The mock does not apply the provider's implicit deletion protection.
    const cluster = expected['google_container_cluster.autopilot[0]'];
    assert.equal(cluster.deletion_protection, null);
    cluster.deletion_protection = true;
    const actual = argumentsByAddress(after[index]);
    if (index === 1) {
      const priorResource = expected['kubernetes_manifest.runner_database_migration[0]'];
      priorResource.computed_fields = ['metadata.annotations', 'metadata.labels', 'spec.template.metadata.labels'];
      const priorJob = priorResource.manifest;
      const job = actual['kubernetes_manifest.runner_database_migration[0]'].manifest;
      assert.notEqual(job.metadata.name, priorJob.metadata.name, 'immutable migration Job must be replaced');
      priorJob.metadata.name = job.metadata.name;
      priorJob.spec.template.spec.containers[0].env.splice(1, 0,
        { name: 'RUNNER_DB_CORE_NAME', value: 'runbook' },
        { name: 'RUNNER_DB_NAME', value: 'runbook_runner' });
      priorJob.spec.template.spec.containers[0].env.push(
        { name: 'RUNNER_DB_CONNECTION_LIMITS', value: '{"api":9,"bootstrap":2,"vault":5}' },
        { name: 'RUNNER_DB_CORE_CONNECTIONS', value: '24' },
        { name: 'RUNNER_INSTALL_GROUP_ID', value: 'fixture' },
        { name: 'RUNNER_INSTALL_RUNNER_ID', value: 'fixture' },
        { name: 'RUNNER_INSTALL_WORKER_CAPACITY', value: '1' });
      const apiEnv = expected['kubernetes_manifest.runner["api"]'].manifest.spec.template.spec.containers[0].env;
      apiEnv.push(
        { name: 'RUNNER_DB_POOL_SIZE', value: '4' },
        { name: 'RUNNER_SANDBOX_IMAGE', value: `ghcr.io/angler-ai/runbook-sandbox@sha256:${'c'.repeat(64)}` },
        { name: 'RUNNER_SANDBOX_APPROVED_IMAGES', value: `ghcr.io/angler-ai/runbook-sandbox@sha256:${'c'.repeat(64)}` },
        { name: 'RUNNER_GKE_WORKER_URL', value: 'http://runner-worker.runbook-runner.svc.cluster.local:8094' },
        { name: 'RUNNER_INSTALL_WORKER_CAPACITY', value: '1' });
      apiEnv.sort((a, b) => a.name < b.name ? -1 : Number(a.name > b.name));
      const vaultEnv = expected['kubernetes_manifest.runner["vault"]'].manifest.spec.template.spec.containers[0].env;
      vaultEnv.push({ name: 'RUNNER_DB_POOL_SIZE', value: '4' });
      vaultEnv.sort((a, b) => a.name < b.name ? -1 : Number(a.name > b.name));
      const approval = actual['kubernetes_config_map_v1.sandbox_images[0]'];
      assert.deepEqual(Object.values(approval.data), [`ghcr.io/angler-ai/runbook-sandbox@sha256:${'c'.repeat(64)}`]);
      assert.equal(approval.metadata[0].namespace, 'runbook-sandboxes');
      delete actual['kubernetes_config_map_v1.sandbox_images[0]'];
      expected['kubernetes_role_v1.sandbox_ctl[0]'].rule.push({
        api_groups: [''], resource_names: ['sandbox-images'], resources: ['configmaps'], verbs: ['get'],
      });
      const admission = expected['kubernetes_manifest.sandbox_policy[0]'].manifest;
      admission.spec.paramKind = { apiVersion: 'v1', kind: 'ConfigMap' };
      expected['kubernetes_manifest.sandbox_policy_binding[0]'].manifest.spec.paramRef = {
        name: 'sandbox-images', namespace: 'runbook-sandboxes', parameterNotFoundAction: 'Deny',
      };
      const prior = admission.spec.validations;
      const image = prior.find(({ expression }) => expression.includes('c.image =='));
      image.expression = image.expression.replace(/c\.image == '[^']+'/,
        'params != null && has(params.data) && params.data.exists(k, params.data[k] == c.image)');
      const scheduler = prior.find(({ expression }) => expression.includes('schedulerName'));
      scheduler.expression = scheduler.expression.replace("== 'default-scheduler'",
        "in ['default-scheduler', 'gke.io/optimize-utilization-scheduler']");
    }
    assert.deepEqual(actual, expected);
  }
});

test('a prefixed, labeled runner plans without public ingress', (t) => {
  const terraform = findTerraform();
  if (!terraform) return t.skip('terraform binary not found');

  const [plan] = planFixture(t, terraform, `
run "temporary" {
  command = plan
  expect_failures = [check.runner_oauth_https]
  variables {
    runner_enabled = true
    database_auth = "iam"
    ${enrolledRunnerInputs}
    name_prefix = "runbook-test-"
    labels = { runbook-test = "true" }
    release_channel = "RAPID"
    deletion_protection = false
    replicas = 1
    sql_tier = "db-f1-micro"
    sql_final_backup = false
    sql_retain_backups_on_delete = false
    sql_deletion_protection = false
    sql_api_deletion_protection = false
    kms_prevent_destroy = false
    ingress_enabled = false
    runner_image = "us-central1-docker.pkg.dev/runbook-plan-fixture/runbook-test-images/runbook-test-runbook-runner@sha256:${'b'.repeat(64)}"
    runner_sandbox_image = "us-central1-docker.pkg.dev/runbook-plan-fixture/runbook-test-images/runbook-test-runbook-sandbox@sha256:${'c'.repeat(64)}"
    runner_sandbox_images = ["ghcr.io/example/sandbox@sha256:${'d'.repeat(64)}"]
    runner_registry_https_cidrs = ["192.0.2.1/32"]
  }
}
`);
  const resources = Object.fromEntries(plan.resource_changes
    .filter(({ mode }) => mode === 'managed')
    .map(({ address, change }) => [address, change.after]));
  const cluster = resources['google_container_cluster.autopilot[0]'];
  assert.equal(cluster.name, 'runbook-test-runbook-cluster');
  assert.equal(cluster.resource_labels['runbook-test'], 'true');
  assert.equal(cluster.release_channel[0].channel, 'RAPID');
  assert.equal(cluster.deletion_protection, false);
  const policy = resources['kubernetes_manifest.sandbox_policy[0]'].manifest;
  // Exact post-mutation scheduler observed on the admitted gVisor pod.
  const trustedRules = policy.spec.validations.map(({ expression }) => expression).filter((rule) => rule.includes('trusted-helper'));
  assert.equal(trustedRules.length, 1);
  assert.match(trustedRules[0], /us-central1-docker\.pkg\.dev\/runbook-plan-fixture\/runbook-test-images\/runbook-test-runbook-sandbox@sha256:/);
  assert.ok(policy.spec.validations.some(({ expression }) => expression.includes("m.mountPath == '/runbook-trusted' && m.readOnly")));
  const schedulerRule = policy.spec.validations.find(({ expression }) => expression.includes('schedulerName')).expression;
  assert.match(schedulerRule, /schedulerName in \['default-scheduler', 'gke[.]io\/optimize-utilization-scheduler'\]/);
  for (const [address, resource] of Object.entries(resources)) {
    if (/^google_sql_(database|user)\./.test(address)) {
      assert.equal(resource.deletion_policy, 'ABANDON', address);
    }
  }
  const env = (address) => Object.fromEntries(resources[address].manifest.spec.template.spec.containers[0].env
    .map(({ name, value }) => [name, value]));
  const approved = Object.values(resources['kubernetes_config_map_v1.sandbox_images[0]'].data);
  assert.deepEqual(new Set(approved), new Set([
    `us-central1-docker.pkg.dev/runbook-plan-fixture/runbook-test-images/runbook-test-runbook-sandbox@sha256:${'c'.repeat(64)}`,
    `ghcr.io/example/sandbox@sha256:${'d'.repeat(64)}`,
  ]));
  assert.equal(resources['kubernetes_manifest.sandbox_policy_binding[0]'].manifest.spec.paramRef.name, 'runbook-test-sandbox-images');
  assert.deepEqual(new Set(env('kubernetes_manifest.runner["api"]').RUNNER_SANDBOX_APPROVED_IMAGES.split(',')), new Set(approved));
  const registryNetwork = resources['kubernetes_network_policy_v1.runner_registry_resolution[0]'].spec[0];
  assert.deepEqual(registryNetwork.pod_selector[0].match_labels, { 'runbook.dev/process': 'api' });
  assert.deepEqual(registryNetwork.egress[0].to[0].ip_block[0].cidr, '192.0.2.1/32');
  assert.deepEqual(registryNetwork.egress[0].ports[0].port, '443');
  const sandboxEnv = env('kubernetes_manifest.runner["sandbox-ctl"]');
  assert.equal(sandboxEnv.RUNNER_SANDBOX_NAME_PREFIX, 'runbook-test-');
  assert.equal(sandboxEnv.RUNNER_SANDBOX_SERVICE_ACCOUNT, 'runbook-test-sandbox');
  assert.deepEqual(JSON.parse(sandboxEnv.RUNNER_SANDBOX_LABELS), { 'runbook-test': 'true' });
  const migrationEnv = env('kubernetes_manifest.runner_database_migration[0]');
  assert.equal(migrationEnv.RUNNER_DB_CORE_NAME, 'runbook-test-runbook');
  assert.equal(migrationEnv.RUNNER_DB_NAME, 'runbook-test-runbook_runner');
  assert.equal(migrationEnv.RUNNER_DB_CORE_CONNECTIONS, '16');
  assert.deepEqual(JSON.parse(migrationEnv.RUNNER_DB_CONNECTION_LIMITS), { api: 2, vault: 1, bootstrap: 2 });
  assert.equal(env('kubernetes_manifest.runner["api"]').RUNNER_DB_POOL_SIZE, '1');
  assert.equal(env('kubernetes_manifest.runner["vault"]').RUNNER_DB_POOL_SIZE, '1');
  const sql = resources['google_sql_database_instance.postgres'].settings[0];
  assert.equal(sql.final_backup_config[0].enabled, false);
  assert.equal(sql.retain_backups_on_delete, false);
  assert.equal(sql.user_labels['runbook-test'], 'true');
  for (const [address, resource] of Object.entries(resources)) {
    assert.doesNotMatch(address, /ingress_ip|runbook_ingress|backend_config|frontend_config|managed_certificate/);
    if (/^google_dns_record_set\./.test(address)) continue;
    const name = resource.account_id ?? resource.metadata?.[0]?.name ?? resource.manifest?.metadata?.name ?? resource.name;
    if (name) assert.match(name, /^runbook-test-/, address);
    if (address.startsWith('google_service_account.')) assert.ok(name.length <= 30, address);
    for (const labels of [resource.labels, resource.metadata?.[0]?.labels, resource.manifest?.metadata?.labels]) {
      if (labels) assert.equal(labels['runbook-test'], 'true', address);
    }
  }
  assert.equal(resources['google_kms_crypto_key.runner_vault_temporary[0]'].name, 'runbook-test-vault');
  assert.equal(resources['google_kms_crypto_key.runner_vault[0]'], undefined);
});

test('runner enrollment fails if the IAM probe returns unsafe', (t) => {
  const terraform = findTerraform();
  if (!terraform) return t.skip('terraform binary not found');

  planFixture(t, terraform, `
run "unsafe_iam" {
  command = apply
  plan_options {
    target = [terraform_data.runner_setup]
  }
  variables {
    runner_enabled = true
    database_auth = "iam"
    ${enrolledRunnerInputs}
  }
  override_data {
    target = data.external.runner_sandbox_iam[0]
    values = { result = { safe = "false" } }
  }
  expect_failures = [terraform_data.runner_setup]
}
`);
});
