// UPD1: cloudflare/scripts/update.mjs's planning and running, with a
// mocked wrangler (no Cloudflare account is touched): which commands run in
// which order, the refusals, and the rollback. LOCAL ONLY.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { cloudflareBundle } from '../../scripts/release/release.mjs';
import { EXIT, compare, latestTag, main, parseArgs, schemaHeads } from '../scripts/update.mjs';

const HEADS = { d1: 1018, execution_do: 5, org_do: 1, webhook_do: 2 };
const TAGS = [
  { tag: 'v1', new_sqlite_classes: ['ExecutionDO'] },
  { tag: 'v2', new_sqlite_classes: ['OrgDO'] },
];

/** A deployment as the mocks see it; `opts` override pieces. */
function world(opts = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'upd1-cf-'));
  const migrations = join(dir, 'migrations');
  mkdirSync(migrations);
  for (const f of opts.files ?? ['0001_history.sql', '1014_run_release_backoff.sql', '1015_new.sql']) {
    writeFileSync(join(migrations, f), '-- test\n');
  }
  const api = join(dir, 'wrangler.prod.jsonc');
  const auth = join(dir, 'wrangler.auth.prod.jsonc');
  const configs = {
    [api]: {
      name: 'runbook-api',
      vars: { ORIGIN: 'https://app.example.test' },
      migrations: opts.tags ?? TAGS,
      d1_databases: [{ binding: 'DB', database_name: 'runbook-production', migrations_dir: migrations, ...opts.binding }],
      ...(opts.assets ? { assets: { directory: 'build-assets' } } : {}),
    },
    [auth]: { name: 'runbook-auth', d1_databases: [{ binding: 'DB', database_name: opts.authDatabase ?? 'runbook-production', migrations_dir: '../auth/drizzle/sqlite' }] },
  };
  const calls = [];
  const state = {
    loggedIn: true,
    deployedVersion: opts.deployed ?? '0.1.0',
    target: opts.target ?? '0.2.0',
    // The Worker's runner applied these versions; wrangler recorded these names.
    runner: opts.runner ?? [1, 1014],
    wranglerNames: opts.wranglerNames ?? [],
    fail: opts.fail ?? null,
    live: opts.deployed ?? '0.1.0',
  };
  const ok = (stdout = '') => ({ status: 0, stdout, stderr: '' });
  const deps = {
    exists: (p) => p in configs,
    readConfig: (p) => (p.endsWith('wrangler.jsonc.example') ? { migrations: TAGS } : structuredClone(configs[p])),
    readFile: (p) => (p.endsWith('do-classes.json') ? JSON.stringify(opts.doClasses ?? [{ tag: 'v1', release: '0.0.0' }]) : `[workspace.package]\nversion = "${state.target}"\n`),
    schemaHeads: () => HEADS,
    rollbackFloor: () => opts.floors ?? { d1: opts.floor ?? null },
    writeFile: (p, text) => calls.push({ kind: 'write', command: p, text }),
    mkdirp: () => {},
    wrangler: (args, options = {}) => {
      const command = args.filter((a) => !a.startsWith('/') && !a.includes(dir)).join(' ');
      calls.push({ kind: 'wrangler', args, command, env: options.env ?? {} });
      if (state.fail && command.startsWith(state.fail)) return { status: 1, stdout: '', stderr: 'boom' };
      if (opts.failApiDeploy && args[0] === 'deploy' && !args.some((a) => a.includes('auth'))) return { status: 1, stdout: '', stderr: 'boom' };
      if (args[0] === 'whoami') return state.loggedIn ? ok('{}') : { status: 1, stdout: '', stderr: '' };
      if (args[0] === 'deployments') {
        const worker = args.some((a) => a.includes('auth')) ? 'auth' : 'api';
        return ok(JSON.stringify({ versions: [{ version_id: `${worker}-previous`, percentage: 100 }] }));
      }
      if (args[0] === 'd1' && args[1] === 'execute' && args.includes('--json')) {
        const sql = args[args.indexOf('--command') + 1];
        const results = sql.includes('sqlite_master')
          ? [{ name: 'schema_migration' }, ...(state.wranglerNames.length ? [{ name: opts.binding?.migrations_table ?? 'd1_migrations' }] : [])]
          : sql.includes('schema_migration')
            ? state.runner.map((version) => ({ version }))
            : state.wranglerNames.map((name) => ({ name }));
        return ok(JSON.stringify([{ results, success: true }]));
      }
      if (args[0] === 'd1' && args[1] === 'time-travel' && args[2] === 'info') return ok(JSON.stringify(opts.bookmarkAnswer ?? { bookmark: `${args[3]}-bookmark` }));
      const isApi = !args.some((a) => a.includes('auth'));
      if (args[0] === 'deploy' && isApi && !opts.deployDoesNothing) state.live = state.target;
      if (args[0] === 'rollback' && isApi) state.live = state.deployedVersion;
      return ok();
    },
    run: (cmd) => {
      calls.push({ kind: 'run', command: cmd.slice(1).join(' ') });
      return state.fail && cmd.join(' ').includes(state.fail) ? 1 : 0;
    },
    git: () => ({ status: 0, stdout: '' }),
    fetchJson: async (url) => {
      calls.push({ kind: 'fetch', command: new URL(url).pathname });
      if (url.endsWith('/.well-known/runbook')) return {};
      return { version: state.live, schema: state.live === state.target ? (opts.heads ?? HEADS) : { d1: 1 } };
    },
    confirm: async () => true,
    sleep: async () => {},
    log: (m) => calls.push({ kind: 'log', command: m }),
    isTTY: false,
  };
  const run = (argv = []) => main(['--source', 'checkout', '--config', api, '--auth-config', auth, '--yes', '--verify-timeout', '1', ...argv], deps);
  const commands = (kind = 'wrangler') => calls.filter((c) => c.kind === kind).map((c) => c.command);
  const logs = () => commands('log').join('\n');
  return { run, calls, commands, logs, state, deps, api, auth, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}

test('the update runs build, bookmark, migrations, deploys and verify in the documented order', async () => {
  const w = world();
  try {
    assert.equal(await w.run(), EXIT.OK, w.logs());
    assert.deepEqual(w.commands(), [
      'whoami --json',
      'deployments status --json -c',
      'deployments status --json -c',
      `d1 execute DB --remote --json --command SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('schema_migration', 'd1_migrations') -c`,
      'd1 execute DB --remote --json --command SELECT version FROM schema_migration -c',
      'd1 time-travel info runbook-production --json -c',
      // The runner applied 0001 and 1014: wrangler is told, so it applies only 1015.
      `d1 execute DB --remote --command CREATE TABLE IF NOT EXISTS d1_migrations(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE, applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL); INSERT OR IGNORE INTO d1_migrations (name) VALUES ('0001_history.sql'), ('1014_run_release_backoff.sql'); -c`,
      'd1 migrations apply DB --remote -c',
      'd1 migrations apply DB --remote -c',
      'deploy -c',
      'deploy -c',
    ]);
    const wrangler = w.calls.filter((c) => c.kind === 'wrangler');
    // The sign-in Worker deploys before the api Worker; each migration step
    // runs non-interactively (the update already asked).
    const deploys = wrangler.filter((c) => c.args[0] === 'deploy').map((c) => c.args[2]);
    assert.match(deploys[0], /auth/);
    assert.doesNotMatch(deploys[1], /auth/);
    const applies = wrangler.filter((c) => c.args[1] === 'migrations');
    assert.doesNotMatch(applies[0].args.at(-1), /auth/);
    assert.match(applies[1].args.at(-1), /auth/);
    assert.ok(applies.every((c) => c.env.CI === '1'));
    assert.deepEqual(w.commands('run'), ['scripts/build-worker.mjs worker', 'scripts/build-worker.mjs auth']);
    assert.deepEqual(w.commands('fetch'), ['/v1/meta', '/v1/meta', '/.well-known/runbook']);
    assert.match(w.logs(), /restore command: npx wrangler d1 time-travel restore runbook-production --bookmark=runbook-production-bookmark/);
    assert.match(w.logs(), /DB \(runbook-production\): 1 pending \(1015_new\.sql\); 2 applied by the Worker/);
    assert.match(w.logs(), /Updated 0\.1\.0 -> 0\.2\.0/);
  } finally {
    w.cleanup();
  }
});

test('the web UI is built when the config serves assets; files wrangler knows are not recorded twice', async () => {
  const w = world({ assets: true, wranglerNames: ['0001_history.sql', '1014_run_release_backoff.sql'], binding: { migrations_table: 'applied' } });
  try {
    assert.equal(await w.run(['--no-auth']), EXIT.OK, w.logs());
    assert.deepEqual(w.commands('run'), ['scripts/build-worker.mjs worker', 'scripts/build-wasm.mjs', 'run build', 'scripts/prepare-assets.mjs']);
    assert.ok(w.commands().some((c) => c.includes("name IN ('schema_migration', 'applied')")));
    assert.ok(w.commands().includes('d1 execute DB --remote --json --command SELECT name FROM applied -c'));
    assert.ok(!w.commands().some((c) => c.includes('INSERT')), 'nothing to record');
    assert.ok(!w.commands().some((c) => c.includes('auth')));
    assert.equal(w.commands().filter((c) => c === 'deploy -c').length, 1);
  } finally {
    w.cleanup();
  }
});

test('refusals change nothing', async () => {
  const cases = [
    [{ deployed: '0.3.0' }, /downgrades are refused/],
    [{ tags: [TAGS[0]] }, /lacks Durable Object migrations[\s\S]*"tag":"v2"/],
  ];
  for (const [opts, message] of cases) {
    const w = world(opts);
    try {
      assert.equal(await w.run(), EXIT.REFUSED, w.logs());
      assert.match(w.logs(), message);
      assert.ok(!w.commands().some((c) => /^(deploy -c|d1 migrations|d1 time-travel|rollback)/.test(c)), w.commands().join('\n'));
      assert.deepEqual(w.commands('run'), []);
    } finally {
      w.cleanup();
    }
  }
  const out = world();
  try {
    out.state.loggedIn = false;
    assert.equal(await out.run(), EXIT.REFUSED);
    assert.match(out.logs(), /not logged in/);
    assert.deepEqual(out.commands(), ['whoami --json']);
  } finally {
    out.cleanup();
  }
  const missing = world();
  try {
    assert.equal(await main(['--source', 'checkout', '--config', '/nowhere/wrangler.prod.jsonc', '--no-auth', '--yes'], { ...missingDeps(missing) }), EXIT.REFUSED);
  } finally {
    missing.cleanup();
  }
  const same = world({ deployed: '0.2.0' });
  try {
    assert.equal(await same.run(), EXIT.OK);
    assert.match(same.logs(), /already runs 0\.2\.0: nothing to do/);
    assert.ok(!same.commands().some((c) => /^(deploy -c|d1 migrations)/.test(c)));
  } finally {
    same.cleanup();
  }
  const again = world({ deployed: '0.2.0' });
  try {
    assert.equal(await again.run(['--redeploy']), EXIT.OK, again.logs());
    assert.ok(again.commands().some((c) => /^deploy -c/.test(c)), 'redeploy deploys the same version');
  } finally {
    again.cleanup();
  }
  const build = world({ fail: 'scripts/build-worker.mjs auth' });
  try {
    assert.equal(await build.run(), EXIT.REFUSED);
    assert.ok(!build.commands().some((c) => /^(deploy -c|d1 migrations|d1 time-travel)/.test(c)));
  } finally {
    build.cleanup();
  }
});

function missingDeps(w) {
  const logs = [];
  return {
    exists: () => false,
    log: (m) => logs.push(m),
    isTTY: false,
    wrangler: () => assert.fail('no wrangler call before the configs are found'),
    get logs() {
      return logs;
    },
    _w: w,
  };
}

test('--dry-run makes only read-only calls', async () => {
  const w = world();
  try {
    assert.equal(await w.run(['--dry-run']), EXIT.OK, w.logs());
    for (const c of w.commands()) {
      assert.match(c, /^(whoami|deployments status|d1 execute DB --remote --json --command SELECT)/, c);
    }
    assert.deepEqual(w.commands('run'), []);
    assert.match(w.logs(), /Dry run: nothing changed/);
    assert.match(w.logs(), /rollback points: auth auth-previous, api api-previous/);
  } finally {
    w.cleanup();
  }
});

test('a failed verify rolls back each deployed Worker, api first, to the recorded version', async () => {
  const w = world({ deployDoesNothing: true });
  try {
    assert.equal(await w.run(), EXIT.ROLLED_BACK, w.logs());
    const rollbacks = w.calls.filter((c) => c.kind === 'wrangler' && c.args[0] === 'rollback');
    assert.deepEqual(rollbacks.map((c) => c.args[1]), ['api-previous', 'auth-previous']);
    assert.ok(rollbacks.every((c) => c.args.includes('--yes')));
    assert.match(w.logs(), /reports release 0\.1\.0, not release 0\.2\.0/);
    assert.match(w.logs(), /the schema stays/);
  } finally {
    w.cleanup();
  }
});

test('a failed api deploy rolls both back; a failed migration deploys nothing', async () => {
  const deploy = world({ fail: 'deploy -c' });
  try {
    // The first deploy (auth) fails: nothing was deployed, so nothing is rolled back.
    assert.equal(await deploy.run(), EXIT.FAILED, deploy.logs());
    assert.ok(!deploy.commands().some((c) => c.startsWith('rollback')));
  } finally {
    deploy.cleanup();
  }
  const api = world({ failApiDeploy: true });
  try {
    // The api deploy fails after the sign-in Worker's succeeded: only that one is rolled back.
    assert.equal(await api.run(), EXIT.ROLLED_BACK, api.logs());
    const rollbacks = api.calls.filter((c) => c.kind === 'wrangler' && c.args[0] === 'rollback');
    assert.deepEqual(rollbacks.map((c) => c.args[1]), ['auth-previous']);
  } finally {
    api.cleanup();
  }
  const migrate = world({ fail: 'd1 migrations apply' });
  try {
    assert.equal(await migrate.run(), EXIT.FAILED, migrate.logs());
    assert.ok(!migrate.commands().some((c) => /^(deploy -c|rollback)/.test(c)));
    assert.match(migrate.logs(), /partly migrated schema/);
    assert.match(migrate.logs(), /time-travel restore runbook-production --bookmark=runbook-production-bookmark/);
  } finally {
    migrate.cleanup();
  }
});

test('versions, tags, arguments and the checkout heads', () => {
  assert.equal(compare('0.2.0', '0.10.0'), -1);
  assert.equal(compare('1.0.0', 'unknown'), 1);
  assert.equal(latestTag(['v0.2.0', 'v0.10.0', 'v0.9.9', 'vnext', 'release']), 'v0.10.0');
  assert.equal(parseArgs(['--verify-timeout', '5']).verifyTimeoutMs, 5000);
  assert.throws(() => parseArgs(['--bogus']), /unknown option/);
  const heads = schemaHeads();
  assert.ok(heads.d1 >= 1014 && heads.execution_do >= 5 && heads.org_do >= 1 && heads.webhook_do >= 2, JSON.stringify(heads));
});

test('--to checks the tag out, installs and runs its own script; a failure checks the previous ref out again', async () => {
  const calls = [];
  const git = (args) => {
    calls.push(['git', ...args].join(' '));
    const out = {
      status: '',
      tag: 'v0.1.0\nv0.2.0\n',
      show: '[workspace.package]\nversion = "0.2.0"\n',
      'symbolic-ref': 'main\n',
    }[args[0]];
    return { status: 0, stdout: out ?? '' };
  };
  let childCode = 0;
  const deps = {
    git,
    run: (cmd) => {
      calls.push(cmd.slice(cmd[0] === 'npm' ? 0 : 1).join(' '));
      return cmd.join(' ').includes('update.mjs') ? childCode : 0;
    },
    log: () => {},
    isTTY: false,
  };
  assert.equal(await main(['--source', 'checkout', '--to', 'latest', '--yes', '--no-auth'], deps), EXIT.OK);
  assert.deepEqual(calls.filter((c) => !c.startsWith('git status') && !c.startsWith('git tag')), [
    'git show v0.2.0:Cargo.toml',
    'git symbolic-ref -q --short HEAD',
    'git checkout --quiet --detach v0.2.0',
    'npm ci --ignore-scripts --no-audit --no-fund',
    'npm ci --ignore-scripts --no-audit --no-fund',
    'npm ci --ignore-scripts --no-audit --no-fund',
    `${calls.find((c) => c.includes('update.mjs'))}`,
  ]);
  assert.match(calls.find((c) => c.includes('update.mjs')), /update\.mjs --resumed-from main --source checkout --yes --no-auth$/);
  calls.length = 0;
  childCode = EXIT.ROLLED_BACK;
  assert.equal(await main(['--source', 'checkout', '--to', 'v0.2.0', '--yes'], deps), EXIT.ROLLED_BACK);
  assert.equal(calls.at(-1), 'git checkout --quiet main');
  calls.length = 0;
  assert.equal(await main(['--source', 'checkout', '--to', 'v9.9.9', '--yes'], deps), EXIT.REFUSED);
  assert.ok(!calls.some((c) => c.includes('checkout')));
});

// --- the release source and the bundle it runs ------------------------------

/** A release bundle built by scripts/release/release.mjs from stand-in build outputs. */
function fakeBundle(version) {
  const root = mkdtempSync(join(tmpdir(), 'upd1-bundle-src-'));
  const out = mkdtempSync(join(tmpdir(), 'upd1-bundle-out-'));
  const put = (path, text = path) => {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  };
  put('Cargo.toml', `[workspace.package]\nversion = "${version}"\n`);
  for (const f of ['build/index.js', 'wrangler.jsonc.example', 'wrangler.auth.jsonc.example', 'package.json', 'package-lock.json',
    'README.md', 'scripts/update.mjs', 'scripts/mfa-reset.mjs', 'migrations/d1/0001_history.sql', 'migrations/d1/1014_x.sql', 'migrations/do/0005_x.sql',
    'migrations/orgdo/0001_x.sql', 'migrations/webhookdo/0002_x.sql']) {
    put(`cloudflare/${f}`);
  }
  put('cloudflare/migrations/do-classes.json', '[{ "tag": "v1", "release": "0.0.0" }]');
  put('crates/runbook-store-postgres/migrations/0001_history.sql');
  put('auth/dist/worker.mjs');
  put('auth/drizzle/sqlite/0000_auth.sql');
  put('web/dist/index.html');
  const file = cloudflareBundle(out, { root }).finish(join(root, 'web', 'dist'));
  rmSync(root, { recursive: true, force: true });
  return { file, bytes: readFileSync(file), cleanup: () => rmSync(out, { recursive: true, force: true }) };
}

function releaseDeps(bundle, { sums, releases, operatorRoot, attestation = 'good' }) {
  const calls = [];
  const temps = [];
  const deps = {
    root: operatorRoot,
    isTTY: false,
    log: (m) => calls.push(`log ${m}`),
    exists: existsSync,
    readFile: (p) => readFileSync(p, 'utf8'),
    writeFile: (p, data) => writeFileSync(p, data),
    githubHeaders: (accept) => ({ accept }),
    hasGh: () => attestation !== 'no-gh',
    verifyAttestation: (file, repo, pins) => {
      calls.push(`attest ${file.split('/').pop()} ${repo} ${pins.signerWorkflow} ${pins.sourceRef}`);
      return attestation === 'bad' ? { status: 1, stderr: 'gh: no attestations matched the signer workflow' } : { status: 0, stderr: '' };
    },
    mkdirp: () => {},
    fetchJson: async (url) => {
      calls.push(`fetch ${url}`);
      return releases;
    },
    download: async (url) => {
      calls.push(`download ${url}`);
      return url === 'sums' ? Buffer.from(sums) : bundle.bytes;
    },
    tempDir: () => {
      const d = mkdtempSync(join(tmpdir(), 'upd1-release-'));
      temps.push(d);
      return d;
    },
    removeDir: (d) => rmSync(d, { recursive: true, force: true }),
    run: (cmd, cwd) => {
      if (cmd[0] === 'tar') return spawnSync(cmd[0], cmd.slice(1)).status;
      calls.push(`run ${cmd[0] === 'npm' ? cmd.join(' ') : cmd.slice(1).join(' ')} @ ${cwd}`);
      return 0;
    },
  };
  return { deps, calls, temps };
}

test('the release source verifies the bundle against SHA256SUMS and hands over to its own update.mjs', async () => {
  const bundle = fakeBundle('0.3.0');
  const operatorRoot = mkdtempSync(join(tmpdir(), 'upd1-operator-'));
  const asset = (v) => [
    { name: `runbook-cloudflare-${v}.tar.gz`, url: 'bundle' },
    { name: 'SHA256SUMS', url: 'sums' },
  ];
  const releases = [
    { tag_name: 'v0.2.9', assets: asset('0.2.9') },
    { tag_name: 'v0.3.0', assets: asset('0.3.0') },
    { tag_name: 'v0.4.0', prerelease: true, assets: asset('0.4.0') },
    { tag_name: 'v0.5.0', draft: true, assets: asset('0.5.0') },
  ];
  const hash = createHash('sha256').update(bundle.bytes).digest('hex');
  try {
    const good = releaseDeps(bundle, { sums: `${hash}  runbook-cloudflare-0.3.0.tar.gz\n`, releases, operatorRoot });
    assert.equal(await main(['--yes', '--release-repo', 'example/runbook', '--url', 'https://app.example.test'], good.deps), EXIT.OK);
    assert.equal(good.calls[0], 'fetch https://api.github.com/repos/example/runbook/releases?per_page=100');
    assert.deepEqual(good.calls.filter((c) => c.startsWith('download')), ['download sums', 'download bundle']);
    const runs = good.calls.filter((c) => c.startsWith('run '));
    const bundleDir = join(good.temps[0], 'runbook-cloudflare-0.3.0', 'cloudflare');
    assert.deepEqual(runs, [
      `run npm ci --ignore-scripts --no-audit --no-fund @ ${bundleDir}`,
      `run ${join(bundleDir, 'scripts', 'update.mjs')} --source bundle --resumed-from v0.3.0 --yes --url https://app.example.test ` +
        `--config ${join(operatorRoot, 'wrangler.prod.jsonc')} --auth-config ${join(operatorRoot, 'wrangler.auth.prod.jsonc')} ` +
        `--state-dir ${join(operatorRoot, '.update')} @ ${bundleDir}`,
    ]);
    assert.ok(good.calls.some((c) => c.includes(`sha256 ${hash} (matches SHA256SUMS)`)));
    // Re-check #1: pinned to the release workflow and the release's tag, not only the repository.
    const attest = 'attest runbook-cloudflare-0.3.0.tar.gz example/runbook example/runbook/.github/workflows/release.yml refs/tags/v0.3.0';
    assert.ok(good.calls.includes(attest), good.calls.join('\n'));
    assert.ok(good.calls.indexOf(attest) < good.calls.findIndex((c) => c.startsWith('run ')));

    for (const [attestation, argv, code, message] of [
      ['bad', [], EXIT.REFUSED, /no valid build attestation[\s\S]*gh: no attestations matched the signer workflow/],
      ['no-gh', [], EXIT.REFUSED, /needs the GitHub CLI/],
      ['no-gh', ['--insecure-skip-verify'], EXIT.OK, /NOT verified[\s\S]*was NOT verified/],
    ]) {
      const r = releaseDeps(bundle, { sums: `${hash}  runbook-cloudflare-0.3.0.tar.gz\n`, releases, operatorRoot, attestation });
      assert.equal(await main(['--yes', ...argv], r.deps), code, r.calls.join('\n'));
      assert.match(r.calls.join('\n'), message);
      if (code !== EXIT.OK) assert.ok(!r.calls.some((c) => c.startsWith('run ')), 'nothing from the bundle ran');
    }
    assert.ok(!existsSync(good.temps[0]), 'the unpacked release is removed afterwards');

    // X.Y picks the newest published release of that line.
    const line = releaseDeps(bundle, { sums: '', releases, operatorRoot });
    assert.equal(await main(['--yes', '--to', '0.2'], line.deps), EXIT.REFUSED);
    assert.ok(line.calls.some((c) => c.includes('==> Release v0.2.9')), line.calls.join('\n'));

    const tampered = releaseDeps(bundle, { sums: `${'0'.repeat(64)}  runbook-cloudflare-0.3.0.tar.gz\n`, releases, operatorRoot });
    assert.equal(await main(['--yes'], tampered.deps), EXIT.REFUSED);
    assert.ok(tampered.calls.some((c) => c.includes('does not match the release')));
    assert.ok(!tampered.calls.some((c) => c.startsWith('run ')), 'nothing installed or run');

    for (const [argv, message] of [
      [['--to', '0.4.0'], /no published release for 0\.4\.0/],
      [['--to', '0.5'], /no published release for 0\.5/],
    ]) {
      const r = releaseDeps(bundle, { sums: '', releases, operatorRoot });
      assert.equal(await main(['--yes', ...argv], r.deps), EXIT.REFUSED);
      assert.match(r.calls.join('\n'), message);
    }
    const none = releaseDeps(bundle, { sums: '', releases: null, operatorRoot });
    assert.equal(await main(['--yes'], none.deps), EXIT.REFUSED);
    assert.match(none.calls.join('\n'), /GITHUB_TOKEN[\s\S]*--source checkout/);
  } finally {
    bundle.cleanup();
    rmSync(operatorRoot, { recursive: true, force: true });
  }
});

test('a release bundle deploys its prebuilt Workers with the operator config beside them, building nothing', async () => {
  const bundle = fakeBundle('0.3.0');
  const unpacked = mkdtempSync(join(tmpdir(), 'upd1-unpacked-'));
  const w = world({ assets: true, target: '0.3.0', heads: { d1: 1014, execution_do: 5, org_do: 1, webhook_do: 2 } });
  try {
    assert.equal(spawnSync('tar', ['-xzf', bundle.file, '-C', unpacked]).status, 0);
    const bundleRoot = join(unpacked, 'runbook-cloudflare-0.3.0', 'cloudflare');
    const written = {};
    const deps = {
      ...w.deps,
      root: bundleRoot,
      exists: (p) => existsSync(p) || w.deps.exists(p),
      readFile: (p) => (p.endsWith('RELEASE.json') ? readFileSync(p, 'utf8') : w.deps.readFile(p)),
      readConfig: (p) => {
        const config = w.deps.readConfig(p);
        if (p === w.api) config.build = { command: 'node scripts/build-worker.mjs worker' };
        return config;
      },
      schemaHeads,
      writeFile: (p, text) => {
        written[p] = JSON.parse(text);
      },
    };
    const code = await main(['--source', 'bundle', '--resumed-from', 'v0.3.0', '--config', w.api, '--auth-config', w.auth, '--yes', '--verify-timeout', '1'], deps);
    assert.equal(code, EXIT.OK, w.logs());
    assert.deepEqual(w.commands('run'), [], 'nothing is built');
    const copy = join(bundleRoot, 'wrangler.api.release.json');
    assert.ok(copy in written && !('build' in written[copy]), 'the config copy has no build command');
    assert.equal(written[copy].vars.ORIGIN, 'https://app.example.test');
    const deploys = w.calls.filter((c) => c.kind === 'wrangler' && c.args[0] === 'deploy').map((c) => c.args[2]);
    assert.deepEqual(deploys, [join(bundleRoot, 'wrangler.auth.release.json'), copy]);
    assert.match(w.logs(), /Updated 0\.1\.0 -> 0\.3\.0/);
  } finally {
    w.cleanup();
    bundle.cleanup();
    rmSync(unpacked, { recursive: true, force: true });
  }
});

// --- data-safety review --------------------------------------------------------

test('bookmarks: each database, the sign-in one too when separate; saved to a file; an empty one stops the update', async () => {
  const w = world({ authDatabase: 'runbook-auth' });
  try {
    assert.equal(await w.run(['--state-dir', '/state']), EXIT.OK, w.logs());
    const infos = w.calls.filter((c) => c.kind === 'wrangler' && c.args[2] === 'info');
    assert.deepEqual(infos.map((c) => c.args[3]), ['runbook-production', 'runbook-auth']);
    assert.match(infos[1].args.at(-1), /auth/);
    const saved = w.calls.filter((c) => c.kind === 'write' && c.command.startsWith('/state/bookmarks-'));
    assert.ok(saved.length, 'bookmarks saved to the state directory');
    assert.deepEqual(JSON.parse(saved.at(-1).text).map((b) => [b.database, b.bookmark]), [
      ['runbook-production', 'runbook-production-bookmark'],
      ['runbook-auth', 'runbook-auth-bookmark'],
    ]);
    // Every bookmark is taken before the first migration.
    const order = w.commands();
    assert.ok(order.findIndex((c) => c.includes('time-travel info runbook-auth')) < order.findIndex((c) => c.startsWith('d1 migrations')));
  } finally {
    w.cleanup();
  }
  for (const answer of [{}, { bookmark: '' }]) {
    const empty = world({ bookmarkAnswer: answer });
    try {
      assert.equal(await empty.run(), EXIT.REFUSED, empty.logs());
      assert.match(empty.logs(), /no Time Travel bookmark/);
      assert.ok(!empty.commands().some((c) => /^(d1 migrations|deploy -c|d1 execute DB --remote --command CREATE)/.test(c)));
    } finally {
      empty.cleanup();
    }
  }
});

test('below the rollback floor: refused without --restore-backup; with it, a failure also restores D1', async () => {
  const refused = world({ floor: '0.1.5' });
  try {
    assert.equal(await refused.run(), EXIT.REFUSED, refused.logs());
    assert.match(refused.logs(), /0\.1\.0 is below 0\.2\.0's rollback floor 0\.1\.5[\s\S]*--restore-backup/);
    assert.ok(!refused.commands().some((c) => /^(d1 migrations|deploy -c|d1 time-travel)/.test(c)));
  } finally {
    refused.cleanup();
  }
  const restore = world({ floor: '0.1.5', deployDoesNothing: true });
  try {
    assert.equal(await restore.run(['--restore-backup']), EXIT.ROLLED_BACK, restore.logs());
    const after = restore.commands().slice(restore.commands().findIndex((c) => c.startsWith('rollback')));
    assert.deepEqual(after, [
      'rollback api-previous -c --message update.mjs: 0.2.0 failed --yes',
      'rollback auth-previous -c --message update.mjs: 0.2.0 failed --yes',
      'd1 time-travel restore runbook-production --bookmark=runbook-production-bookmark -c',
    ]);
    assert.match(restore.logs(), /Durable Object and R2 state is not restored/);
  } finally {
    restore.cleanup();
  }
  const fine = world({ floor: '0.1.0' });
  try {
    assert.equal(await fine.run(), EXIT.OK, fine.logs());
  } finally {
    fine.cleanup();
  }
});

test('a Durable Object class migration: no automatic rollback, said up front', async () => {
  const classes = [{ tag: 'v1', release: '0.0.0' }, { tag: 'v2', release: '0.2.0' }];
  const refused = world({ doClasses: classes });
  try {
    assert.equal(await refused.run(), EXIT.REFUSED, refused.logs());
    assert.match(refused.logs(), /Durable Object class migration v2[\s\S]*cannot roll back across[\s\S]*--accept-no-rollback/);
  } finally {
    refused.cleanup();
  }
  const accepted = world({ doClasses: classes, deployDoesNothing: true });
  try {
    assert.equal(await accepted.run(['--accept-no-rollback']), EXIT.FAILED, accepted.logs());
    assert.ok(!accepted.commands().some((c) => c.startsWith('rollback')), 'wrangler rollback cannot cross it');
    assert.match(accepted.logs(), /no automatic rollback/);
  } finally {
    accepted.cleanup();
  }
  // Classes the deployed release already has never block.
  const old = world({ doClasses: [{ tag: 'v1', release: '0.0.0' }, { tag: 'v2', release: '0.1.0' }] });
  try {
    assert.equal(await old.run(), EXIT.OK, old.logs());
  } finally {
    old.cleanup();
  }
});

test('the checkout\'s Durable Object class tags are all mapped to the release that added them', () => {
  const template = readFileSync(join(dirname(new URL(import.meta.url).pathname), '..', 'wrangler.jsonc.example'), 'utf8');
  const classes = JSON.parse(readFileSync(join(dirname(new URL(import.meta.url).pathname), '..', 'migrations', 'do-classes.json'), 'utf8'));
  const tags = [...template.matchAll(/"tag":\s*"(v\d+)"/g)].map((m) => m[1]);
  assert.ok(tags.length >= 4);
  assert.deepEqual(classes.map((c) => c.tag), tags);
  assert.ok(classes.every((c) => /^\d+\.\d+\.\d+$/.test(c.release)));
});

test('a Durable Object rollback floor: --restore-backup cannot meet it; it needs --accept-no-rollback', async () => {
  const refused = world({ floors: { execution_do: '0.1.5' } });
  try {
    assert.equal(await refused.run(['--restore-backup']), EXIT.REFUSED, refused.logs());
    assert.match(refused.logs(), /Durable Object rollback floor 0\.1\.5 \(execution_do\)[\s\S]*restoring D1 does not undo[\s\S]*--accept-no-rollback/);
    assert.ok(!refused.commands().some((c) => /^(d1 migrations|deploy -c|d1 time-travel)/.test(c)));
  } finally {
    refused.cleanup();
  }
  const accepted = world({ floors: { webhook_do: '0.1.5' }, deployDoesNothing: true });
  try {
    assert.equal(await accepted.run(['--accept-no-rollback']), EXIT.FAILED, accepted.logs());
    assert.ok(!accepted.commands().some((c) => c.startsWith('rollback')), 'no Worker rollback onto unsafe code');
  } finally {
    accepted.cleanup();
  }
  // A D1 floor alone is still met by --restore-backup.
  const d1 = world({ floors: { d1: '0.1.5' } });
  try {
    assert.equal(await d1.run(['--restore-backup']), EXIT.OK, d1.logs());
  } finally {
    d1.cleanup();
  }
});

test('the floor parsers agree on the shared fixture; the checkout floor is per store', async () => {
  const { annotationFloor, rollbackFloor: checkoutFloor } = await import('../scripts/update.mjs');
  const here = dirname(new URL(import.meta.url).pathname);
  const sql = readFileSync(join(here, '..', '..', 'scripts', 'tests', 'fixtures', 'floor-annotations.sql'), 'utf8');
  const expected = JSON.parse(readFileSync(join(here, '..', '..', 'scripts', 'tests', 'fixtures', 'floor-annotations.json'), 'utf8'));
  assert.equal(annotationFloor([sql]), expected.floor);
  const floors = checkoutFloor(join(here, '..', '..'));
  assert.deepEqual(Object.keys(floors).sort(), ['d1', 'execution_do', 'org_do', 'webhook_do']);
});

test('a release bundle whose RELEASE.json has no rollback_floor is refused, not read as "no floors"', async () => {
  const bundle = fakeBundle('0.3.0');
  const unpacked = mkdtempSync(join(tmpdir(), 'upd1-nofloor-'));
  const w = world({ target: '0.3.0' });
  try {
    assert.equal(spawnSync('tar', ['-xzf', bundle.file, '-C', unpacked]).status, 0);
    const top = join(unpacked, 'runbook-cloudflare-0.3.0');
    const manifest = JSON.parse(readFileSync(join(top, 'RELEASE.json'), 'utf8'));
    delete manifest.rollback_floor;
    writeFileSync(join(top, 'RELEASE.json'), JSON.stringify(manifest));
    const deps = {
      ...w.deps,
      root: join(top, 'cloudflare'),
      exists: (p) => existsSync(p) || w.deps.exists(p),
      readFile: (p) => (p.endsWith('RELEASE.json') ? readFileSync(p, 'utf8') : w.deps.readFile(p)),
      schemaHeads,
      writeFile: () => {},
    };
    const code = await main(['--source', 'bundle', '--resumed-from', 'v0.3.0', '--config', w.api, '--auth-config', w.auth, '--yes'], deps);
    assert.equal(code, EXIT.REFUSED, w.logs());
    assert.match(w.logs(), /RELEASE\.json has no rollback_floor/);
    assert.ok(!w.commands().some((c) => /^(d1 migrations|deploy -c)/.test(c)));
  } finally {
    w.cleanup();
    bundle.cleanup();
    rmSync(unpacked, { recursive: true, force: true });
  }
});
