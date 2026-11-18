import { afterEach, describe, expect, it } from 'vitest';
import { lastEditorTab, overviewTab, rememberEditorTab, validateOverviewSearch } from './tabs';

afterEach(() => localStorage.clear());

describe('the runbook page tabs', () => {
  it('defaults to the graph and keeps it out of the URL', () => {
    expect(validateOverviewSearch({})).toEqual({});
    expect(validateOverviewSearch({ tab: 'graph' })).toEqual({});
    expect(overviewTab(validateOverviewSearch({}))).toBe('graph');
  });

  it('accepts the other tabs and ignores unknown ones', () => {
    expect(overviewTab(validateOverviewSearch({ tab: 'runs' }))).toBe('runs');
    expect(overviewTab(validateOverviewSearch({ tab: 'versions' }))).toBe('versions');
    expect(overviewTab(validateOverviewSearch({ tab: 'memory' }))).toBe('memory');
    expect(overviewTab(validateOverviewSearch({ tab: 'archives' }))).toBe('archives');
    expect(overviewTab(validateOverviewSearch({ tab: 'source' }))).toBe('source');
    expect(overviewTab(validateOverviewSearch({ tab: 'yaml' }))).toBe('graph');
    expect(overviewTab(validateOverviewSearch({ tab: 3 }))).toBe('graph');
  });

  it('keeps the run filters but never a runbook filter (the page is one runbook)', () => {
    expect(validateOverviewSearch({ tab: 'runs', status: 'failed', runbook: 'x' })).toEqual({
      tab: 'runs',
      status: 'failed',
    });
  });
});

describe('the editor tab', () => {
  it('opens on the graph unless the person last chose another', () => {
    expect(lastEditorTab()).toBe('graph');
    rememberEditorTab('yaml');
    expect(lastEditorTab()).toBe('yaml');
    rememberEditorTab('nonsense');
    expect(lastEditorTab()).toBe('yaml');
    localStorage.setItem('rb.editor.tab', 'bogus');
    expect(lastEditorTab()).toBe('graph');
  });
});
