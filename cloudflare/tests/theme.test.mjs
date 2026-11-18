// UI-1: the theme colour override on Workers. The RUNBOOK_THEME_* vars are
// read as natively (runbook_api::discovery::theme_from_vars): `/v1/meta`
// reports them as `theme` (light = base, dark = the DARK var else base),
// none when unset. UI-1 review: a value that is not #rrggbb is ignored with
// a logged error (config.invalid_theme_color) and the default theme served:
// the theme is cosmetic, and a Worker has no startup to refuse, so refusing
// would turn a typo into an outage. Natively it still refuses startup.
// LOCAL ONLY.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { startWorker } from './harness.mjs';

async function meta(vars) {
  const worker = await startWorker({ faults: false, vars });
  try {
    const r = await worker.fetch('/v1/meta');
    return { status: r.status, body: await r.json() };
  } finally {
    await worker.dispose();
  }
}

test('no theme vars: /v1/meta has no theme', async () => {
  const r = await meta({ RUNBOOK_THEME_ACCENT: ' ' });
  assert.equal(r.status, 200);
  assert.equal(r.body.theme, undefined);
});

test('theme vars resolve per theme, lowercased', async () => {
  const r = await meta({
    RUNBOOK_THEME_ACCENT: '#4338CA',
    RUNBOOK_THEME_ACCENT_SECONDARY: '#0e7490',
    RUNBOOK_THEME_DARK_ACCENT: '#A5B4FC',
  });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.deepEqual(r.body.theme, {
    light: { accent: '#4338ca', accent_secondary: '#0e7490' },
    dark: { accent: '#a5b4fc', accent_secondary: '#0e7490' },
  });
});

test('an invalid theme colour is ignored (default theme), low contrast is kept', async () => {
  const bad = await meta({ RUNBOOK_THEME_ACCENT: '#4338ca', RUNBOOK_THEME_DARK_ACCENT: '#abc' });
  assert.equal(bad.status, 200, JSON.stringify(bad.body));
  assert.equal(bad.body.theme, undefined);
  assert.equal(typeof bad.body.capabilities, 'object', 'the rest of /v1/meta is served');
  const low = await meta({ RUNBOOK_THEME_ACCENT: '#00cc99' });
  assert.equal(low.status, 200);
  assert.equal(low.body.theme.light.accent, '#00cc99');
});
