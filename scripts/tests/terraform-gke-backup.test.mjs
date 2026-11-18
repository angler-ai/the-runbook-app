import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const tfDir = join(dirname(fileURLToPath(import.meta.url)), '../../deploy/terraform/gcp-gke');
const script = join(tfDir, 'pre-upgrade-backup.sh');

function fakeTools({ statuses = ['SUCCESSFUL'], existing = '' } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'runbook-backup-test-'));
  const put = (name, text) => {
    const path = join(dir, name);
    writeFileSync(path, text);
    chmodSync(path, 0o755);
  };
  put(
    'terraform',
    `#!/bin/sh
if [ "$1" = output ] && [ "$2" = -raw ] && [ "$3" = cloud_sql_connection_name ]; then
  printf '%s\\n' 'example-project:us-central1:runbook-postgres'
  exit 0
fi
echo "unexpected terraform: $*" >&2
exit 9
`,
  );
  put(
    'gcloud',
    `#!/bin/sh
printf '%s\\n' "$*" >> "$FAKE_LOG"
case "$*" in
  *"backups list"*)
    if [ -f "$FAKE_STATE.created" ]; then printf '%s\\n' 1700000000123; else printf '%s' "$FAKE_EXISTING"; fi
    ;;
  *"backups create"*)
    : > "$FAKE_STATE.created"
    printf '%s\\n' 'Creating backup...' >&2
    ;;
  *"backups describe"*)
    count=0
    if [ -f "$FAKE_STATE" ]; then count=$(cat "$FAKE_STATE"); fi
    count=$((count + 1))
    printf '%s' "$count" > "$FAKE_STATE"
    status=$(printf '%s' "$FAKE_STATUSES" | cut -d, -f"$count")
    if [ -z "$status" ]; then status=$(printf '%s' "$FAKE_STATUSES" | awk -F, '{print $NF}'); fi
    printf '%s\\n' "$status"
    ;;
  *)
    echo "unexpected gcloud: $*" >&2
    exit 9
    ;;
esac
`,
  );
  return {
    dir,
    run(id = 'pre-upgrade-test') {
      const log = join(dir, 'gcloud.log');
      const state = join(dir, 'state');
      const out = spawnSync('sh', [script, id], {
        cwd: dir,
        encoding: 'utf8',
        env: {
          ...process.env,
          PATH: `${dir}:${process.env.PATH}`,
          FAKE_LOG: log,
          FAKE_STATE: state,
          FAKE_STATUSES: statuses.join(','),
          FAKE_EXISTING: existing,
          RUNBOOK_BACKUP_POLL_SECONDS: '0',
          RUNBOOK_BACKUP_TIMEOUT_SECONDS: '5',
        },
      });
      let calls = '';
      try {
        calls = readFileSync(log, 'utf8');
      } catch {}
      return { ...out, calls };
    },
  };
}

test('pre-upgrade backup creates a labelled Cloud SQL backup and waits for its assigned id', () => {
  const fake = fakeTools({ statuses: ['RUNNING', 'SUCCESSFUL'] });
  try {
    const out = fake.run();
    assert.equal(out.status, 0, out.stderr);
    assert.equal(out.stdout.trim(), '1700000000123');
    assert.match(
      out.calls,
      /sql backups create --instance=runbook-postgres --project=example-project --description=pre-upgrade-test --async/,
    );
    assert.doesNotMatch(out.calls, /--backup-id/);
    assert.match(out.calls, /backups list .*--filter=description=pre-upgrade-test --format=value\(id\)/);
    assert.match(out.calls, /backups describe 1700000000123 /);
    assert.ok(out.calls.indexOf('backups create') < out.calls.lastIndexOf('backups describe'));
  } finally {
    rmSync(fake.dir, { recursive: true, force: true });
  }
});

test('pre-upgrade backup refuses a label already in use', () => {
  const fake = fakeTools({ existing: '1690000000001' });
  try {
    const out = fake.run();
    assert.notEqual(out.status, 0);
    assert.match(out.stderr, /already exists/);
    assert.doesNotMatch(out.calls, /backups create/);
  } finally {
    rmSync(fake.dir, { recursive: true, force: true });
  }
});

test('pre-upgrade backup fails when Cloud SQL reports failure', () => {
  const fake = fakeTools({ statuses: ['FAILED'] });
  try {
    const out = fake.run();
    assert.notEqual(out.status, 0);
    assert.match(out.stderr, /FAILED/);
  } finally {
    rmSync(fake.dir, { recursive: true, force: true });
  }
});
