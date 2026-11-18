import { describe, expect, it } from 'vitest';
import {
  collectTags,
  encodeTagParam,
  filterByHealth,
  hasRunData,
  healthCounts,
  normalizeRow,
  runHealth,
  slugify,
  sortRows,
  type RunbookRow,
} from './catalog';

const row = (id: string, extra: Partial<RunbookRow> = {}): RunbookRow => ({
  runbook_id: `rb_${id}`,
  slug: id,
  origin: 'ui',
  archived: false,
  title: id,
  tags: [],
  draft_ahead: false,
  active_runs: 0,
  ...extra,
});

const run = (status: NonNullable<RunbookRow['last_run']>['status'], at: number) => ({
  id: `exe_${status}_${at}`,
  status,
  created_at_ms: at,
});

const rows = [
  row('beta', { last_run: run('succeeded', 30), tags: ['deploy'] }),
  row('alpha', { last_run: run('failed', 10), tags: ['deploy', 'db'] }),
  row('gamma', { last_run: run('waiting', 20) }),
  row('delta'),
  row('eps', { last_run: run('running', 40) }),
];

describe('runbook catalog model', () => {
  it('reads health from the last run', () => {
    expect(rows.map(runHealth)).toEqual(['passing', 'failing', 'attention', 'never', 'running']);
    expect(runHealth(row('x', { last_run: run('paused', 1) }))).toBe('attention');
    expect(runHealth(row('x', { last_run: run('cancelled', 1) }))).toBe('other');
  });

  it('counts and filters by health', () => {
    expect(healthCounts(rows)).toEqual({
      all: 5,
      failing: 1,
      attention: 1,
      running: 1,
      passing: 1,
      never: 1,
    });
    expect(filterByHealth(rows, 'failing').map((r) => r.slug)).toEqual(['alpha']);
    expect(filterByHealth(rows, 'all')).toBe(rows);
  });

  it('sorts by name, last run and attention; recent keeps server order', () => {
    expect(sortRows(rows, 'recent')).toBe(rows);
    expect(sortRows(rows, 'name').map((r) => r.slug)).toEqual([
      'alpha',
      'beta',
      'delta',
      'eps',
      'gamma',
    ]);
    expect(sortRows(rows, 'last-run').map((r) => r.slug)).toEqual([
      'eps',
      'beta',
      'gamma',
      'alpha',
      'delta',
    ]);
    expect(sortRows(rows, 'attention').map((r) => r.slug)).toEqual([
      'alpha',
      'gamma',
      'eps',
      'beta',
      'delta',
    ]);
  });

  it('defaults the UI-1 fields an older server omits, but not run data', () => {
    expect(normalizeRow({ runbook_id: 'rb_1', slug: 's', origin: 'ui', archived: false })).toEqual({
      runbook_id: 'rb_1',
      slug: 's',
      origin: 'ui',
      archived: false,
      tags: [],
      draft_ahead: false,
    });
  });

  // The server omits last_run and active_runs from a caller without
  // executions:read; a server that may send them always sends active_runs.
  it('tells withheld run data from a runbook that never ran', () => {
    const withheld = normalizeRow({ runbook_id: 'rb_1', slug: 's', origin: 'ui', archived: false });
    expect(withheld.active_runs).toBeUndefined();
    expect(hasRunData([withheld])).toBe(false);
    expect(hasRunData([withheld, { ...withheld, active_runs: null }])).toBe(false);
    expect(hasRunData([withheld, row('never')])).toBe(true);
    expect(hasRunData([])).toBe(false);
  });

  it('trims and de-duplicates tags, defensively', () => {
    const normalized = normalizeRow({ ...row('x'), tags: ['deploy', ' deploy ', 'db', 'db', ''] });
    expect(normalized.tags).toEqual(['deploy', 'db']);
  });

  it('collects tags, most used first, counting each row once', () => {
    expect(collectTags(rows)).toEqual(['deploy', 'db']);
    expect(
      collectTags([
        row('a', { tags: ['x', 'x', 'x'] }),
        row('b', { tags: ['y'] }),
        row('c', { tags: ['y'] }),
      ]),
    ).toEqual(['y', 'x']);
  });

  it('escapes commas and backslashes in the tag parameter', () => {
    expect(encodeTagParam(['deploy'])).toBe('deploy');
    expect(encodeTagParam(['a,b', 'c\\d', 'e'])).toBe('a\\,b,c\\\\d,e');
  });

  it('slugifies titles the way the server accepts slugs', () => {
    expect(slugify('Post-deploy checks!')).toBe('post-deploy-checks');
    expect(slugify('  Ünïcode — DB  failover ')).toBe('unicode-db-failover');
  });
});
