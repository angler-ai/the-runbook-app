// Runs the TypeScript, Python and Go signature samples on the webhook
// signatures page against its published test vector (Python and Go are
// skipped when their toolchains are missing). The expected value is also
// computed here independently, with node:crypto.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const page = readFileSync(new URL('../../src/content/docs/webhooks/signatures.mdx', import.meta.url), 'utf8');
const block = (title) => {
  const m = page.match(new RegExp('```[a-z]+ title="' + title.replace('.', '\\.') + '"\\n([\\s\\S]*?)\\n```'));
  assert.ok(m, `the page has a ${title} block`);
  return m[1];
};
const row = (label) => page.match(new RegExp('\\| ' + label + ' \\| `([^`]+)` \\|'))[1];

const SECRET = row('Secret');
const TS = row('`Runbook-Timestamp`');
const BODY = row('Body');
const HEADER = row('`Runbook-Signature`');
const NOW = Number(TS);
const tmp = mkdtempSync(join(tmpdir(), 'runbook-sig-'));
process.on('exit', () => rmSync(tmp, { recursive: true, force: true }));

const have = (cmd, args) => {
  try {
    execFileSync(cmd, args, { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
};

test('the published vector is HMAC-SHA256 of "{timestamp}.{body}"', () => {
  const hex = createHmac('sha256', SECRET).update(`${TS}.${BODY}`).digest('hex');
  assert.equal(HEADER, `v1=${hex}`);
});

// [secret, body, timestamp, header, now] cases and whether each must verify.
const cases = [
  ['valid', BODY, TS, HEADER, NOW, true],
  ['valid among rotated entries', BODY, TS, `v1=${'0'.repeat(64)}, ${HEADER}`, NOW, true],
  ['tampered body', BODY.replace('test', 'tesT'), TS, HEADER, NOW, false],
  ['stale timestamp', BODY, TS, HEADER, NOW + 301, false],
  ['timestamp just inside the window', BODY, TS, HEADER, NOW - 300, true],
  ['non-numeric timestamp', BODY, `${TS}a`, HEADER, NOW, false],
  ['no v1 entry', BODY, TS, HEADER.replace('v1=', 'v0='), NOW, false],
];

test('the TypeScript sample', async () => {
  const file = join(tmp, 'verify.ts');
  writeFileSync(file, block('verify.ts'));
  const { verifyRunbookSignature } = await import(pathToFileURL(file).href);
  for (const [name, body, ts, header, now, ok] of cases) {
    assert.equal(verifyRunbookSignature(SECRET, body, ts, header, now), ok, name);
  }
});

test('the Python sample', { skip: !have('python3', ['--version']) && 'python3 is not installed' }, () => {
  writeFileSync(join(tmp, 'verify.py'), block('verify.py'));
  const driver = [
    'import json, sys',
    'sys.path.insert(0, sys.argv[1])',
    'from verify import verify_runbook_signature',
    'secret, cases = json.loads(sys.stdin.read())',
    'print(json.dumps([verify_runbook_signature(secret, b.encode(), t, h, n) for _, b, t, h, n, _ok in cases]))',
  ].join('\n');
  writeFileSync(join(tmp, 'driver.py'), driver);
  const out = execFileSync('python3', ['-I', join(tmp, 'driver.py'), tmp], {
    input: JSON.stringify([SECRET, cases]),
    encoding: 'utf8',
  });
  assert.deepEqual(JSON.parse(out), cases.map((c) => c[5]));
});

test('the Go sample', { skip: !have('go', ['version']) && 'go is not installed' }, () => {
  const dir = join(tmp, 'gomod');
  execFileSync('mkdir', ['-p', dir]);
  writeFileSync(join(dir, 'go.mod'), 'module example.com/runbookhook\n\ngo 1.21\n');
  writeFileSync(join(dir, 'verify.go'), block('verify.go'));
  const lit = (s) => JSON.stringify(s);
  const rows = cases
    .map(([name, b, t, h, n, ok]) => `{${lit(name)}, ${lit(b)}, ${lit(t)}, ${lit(h)}, ${n}, ${ok}},`)
    .join('\n\t\t');
  writeFileSync(
    join(dir, 'verify_test.go'),
    `package runbookhook

import (
	"testing"
	"time"
)

func TestVector(t *testing.T) {
	cases := []struct {
		name, body, ts, header string
		now                    int64
		ok                     bool
	}{
		${rows}
	}
	for _, c := range cases {
		if got := VerifySignature(${lit(SECRET)}, []byte(c.body), c.ts, c.header, time.Unix(c.now, 0)); got != c.ok {
			t.Errorf("%s: got %v, want %v", c.name, got, c.ok)
		}
	}
}
`,
  );
  execFileSync('go', ['test', './...'], {
    cwd: dir,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, GOTOOLCHAIN: 'local', GOFLAGS: '-mod=mod', GOWORK: 'off' },
  });
});
