// Gate fix B1: the test seams are compiled into the test build only
// (`--features test-seams`). The deploy build (`build/`, what the size gate
// measures and an operator deploys) refuses to serve if any of their vars
// is set, rather than ignore a setting someone believed was in force.
// LOCAL ONLY.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DEPLOY, startWorker } from './harness.mjs';

const SEAMS = {
  RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS: '86400000',
  RUNBOOK_TEST_HOLD_WORK: 'on',
  WEBHOOKS_ALLOW_PRIVATE: 'on',
  SSE_KEEPALIVE_MS: '999999999',
  SSE_LIFETIME_MS: '999999999',
  CF1_FAULT_INJECTION: 'on',
  RUNBOOK_TEST_RELEASE: '9.9.9',
  RUNBOOK_TEST_IMPORT_SUBREQUESTS: '1',
};

test('the deploy build serves without test vars', async () => {
  const worker = await startWorker({ worker: DEPLOY, faults: false, vars: {} });
  try {
    const meta = await worker.fetch('/v1/meta');
    assert.equal(meta.status, 200, await meta.text());
  } finally {
    await worker.dispose();
  }
});

for (const [name, value] of Object.entries(SEAMS)) {
  test(`the deploy build refuses to serve with ${name} set`, async () => {
    const worker = await startWorker({ worker: DEPLOY, faults: false, vars: { [name]: value } });
    try {
      for (const path of ['/v1/meta', '/v1/me', '/internal/bootstrap']) {
        const response = await worker.fetch(path);
        assert.equal(response.status, 503, path);
        assert.equal((await response.json()).code, 'config.test_seam_set', path);
      }
    } finally {
      await worker.dispose();
    }
  });
}

test('gate round 2: without the LIMITS binding the Worker refuses to serve (a config error)', async () => {
  for (const worker of [DEPLOY, undefined]) {
    const missing = await startWorker({ worker, faults: false, without: ['LIMITS'] });
    try {
      for (const path of ['/v1/meta', '/v1/device/code']) {
        const response = await missing.fetch(path, { method: path === '/v1/meta' ? 'GET' : 'POST' });
        assert.equal(response.status, 503, path);
        assert.equal((await response.json()).code, 'config.limits_unbound', path);
      }
    } finally {
      await missing.dispose();
    }
  }
});
