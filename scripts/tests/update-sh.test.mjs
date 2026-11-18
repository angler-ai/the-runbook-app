// UPD1 re-check: deploy/update.sh's functions, without Docker. The script is
// sourced with RUNBOOK_UPDATE_LIB=1 (main does not run) and given a fake
// `gh` and `docker` on PATH that record their arguments.
//   node --test scripts/tests/
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const script = join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'deploy', 'update.sh');

/** Run `body` in bash with update.sh's functions loaded and fake tools on PATH. */
function bash(body, { gh = 'exit 0', docker = 'exit 0' } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'upd1-sh-'));
  try {
    for (const [name, code] of Object.entries({ gh, docker })) {
      writeFileSync(join(dir, name), `#!/bin/sh\necho "$0 $*" >> "${dir}/calls"\n${code}\n`);
      chmodSync(join(dir, name), 0o755);
    }
    const out = spawnSync('bash', ['-c', `set -euo pipefail; RUNBOOK_UPDATE_LIB=1 source "${script}"; ${body}`], {
      encoding: 'utf8',
      env: { ...process.env, PATH: `${dir}:${process.env.PATH}` },
    });
    let calls = '';
    try {
      calls = readFileSync(join(dir, 'calls'), 'utf8');
    } catch {}
    return { status: out.status, stdout: out.stdout, stderr: out.stderr, calls: calls.replaceAll(`${dir}/`, '') };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('verification pins the release workflow and the release tag, not only the repository', () => {
  const r = bash('ATTESTATION_REPO=angler-ai/the-runbook-app; verify_image_attestation ghcr.io/angler-ai/runbook sha256:abc 0.3.1');
  assert.equal(r.status, 0, r.stderr);
  assert.equal(
    r.calls.trim(),
    'gh attestation verify oci://ghcr.io/angler-ai/runbook@sha256:abc --repo angler-ai/the-runbook-app ' +
      '--signer-workflow angler-ai/the-runbook-app/.github/workflows/release.yml --source-ref refs/tags/v0.3.1',
  );
});

test('a failed verification shows gh\'s own message (an unauthenticated gh is not a bad attestation)', () => {
  const r = bash('ATTESTATION_REPO=o/r; verify_image_attestation ghcr.io/o/r sha256:abc 0.3.1', {
    gh: 'echo "To get started with GitHub CLI, please run: gh auth login" >&2; exit 4',
  });
  assert.notEqual(r.status, 0);
  assert.match(r.stdout + r.stderr, /gh auth login/);
});

test('the image version label, read from registry metadata, names the tag to verify against', () => {
  const r = bash('image_version_label ghcr.io/o/r@sha256:abc', {
    docker: `echo '{"linux/amd64":{"config":{"Labels":{"org.opencontainers.image.version":"0.3.1","x":"y"}}}}'`,
  });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout.trim(), '0.3.1');
  assert.match(r.calls, /docker buildx imagetools inspect ghcr\.io\/o\/r@sha256:abc --format/);
});

test('a backup\'s age comes from the time in its ID, not the file\'s mtime', () => {
  const r = bash('backup_epoch runbook-0.1.0-20260101T000000Z; backup_epoch runbook-0.1.0-pre-restore-20261113T163411Z; backup_epoch nonsense || echo none');
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.stdout.trim().split('\n'), ['1767225600', '1794587651', 'none']);
});

// A fake `docker` for the restore: answers by what the command asks for.
const fakeDocker = ({ db = 'runbook', freeKb = 10_000_000, stop = 0 } = {}) => `
case "$*" in
  *POSTGRES_DB*printf*|*printf*POSTGRES_DB*) printf '%s' '${db}' ;;
  *pg_database_size*) echo 1073741824 ;;
  *df*-Pk*) printf 'Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/x 1 1 ${freeKb} 1%% /var/lib/postgresql/data\n' ;;
  *" stop "*) exit ${stop} ;;
esac
exit 0`;
const restoreEnv = 'HAS_BUNDLED_DB=1; PROJECT=p; COMPOSE_FILES=(c.yml); ENV_FILE=e; ';

test('a restore refuses the maintenance database, and a data directory without room for a second copy', () => {
  const maintenance = bash(`${restoreEnv} restore_preconditions || echo refused`, { docker: fakeDocker({ db: 'postgres' }) });
  assert.match(maintenance.stdout, /refused/);
  assert.match(maintenance.stderr, /POSTGRES_DB to name a database other than 'postgres'/);
  // 1 GiB database: 1.5 GiB + 256 MiB needed in PGDATA.
  const full = bash(`${restoreEnv} restore_preconditions || echo refused`, { docker: fakeDocker({ freeKb: 1_000_000 }) });
  assert.match(full.stdout, /refused/);
  assert.match(full.stderr, /PostgreSQL's data directory/);
  assert.match(full.calls, /df -Pk/);
  const fine = bash(`${restoreEnv} restore_preconditions && echo ok`, { docker: fakeDocker() });
  assert.match(fine.stdout, /ok/, fine.stderr);
});

test('a restore stops if the server cannot be stopped, before touching any database', () => {
  const dir = mkdtempSync(join(tmpdir(), 'upd1-dump-'));
  try {
    writeFileSync(join(dir, 'b.dump'), 'not empty');
    const r = bash(`${restoreEnv} BACKUP_FILE='${join(dir, 'b.dump')}'; BACKUP_DIR='${dir}'; TARGET_VERSION=0.2.0; restore_backup || echo refused`, {
      docker: fakeDocker({ stop: 1 }),
    });
    assert.match(r.stdout, /refused/);
    assert.match(r.stderr, /could not stop the server: not restoring/);
    assert.doesNotMatch(r.calls, /pg_dump|CREATE DATABASE|pg_restore -U/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('--to X.Y accepts only an X.Y.* release; --to X.Y.Z only itself', () => {
  const r = bash(
    'for pair in "0.3 0.3.7" "0.3 0.4.0" "0.3 0.30.1" "0.3.1 0.3.1" "0.3.1 0.3.2" "latest 9.9.9"; do ' +
      'set -- $pair; release_matches_request "$1" "$2" && echo "$1 $2 yes" || echo "$1 $2 no"; done',
  );
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.stdout.trim().split('\n'), ['0.3 0.3.7 yes', '0.3 0.4.0 no', '0.3 0.30.1 no', '0.3.1 0.3.1 yes', '0.3.1 0.3.2 no', 'latest 9.9.9 yes']);
});
