import { afterEach, describe, expect, it } from 'vitest';
import type { ProjectView } from '../../api/hooks';
import { lastProject, pickProject, rememberProject } from './currentProject';

const prod = { id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' } as ProjectView;
const sandbox = { id: 'prj_2', org_id: 'org_1', slug: 'sandbox', title: 'Sandbox' } as ProjectView;
const projects = [prod, sandbox];

afterEach(() => localStorage.clear());

describe('pickProject', () => {
  it('prefers the route, then the execution, then the remembered project, then the first', () => {
    expect(
      pickProject({ projects, route: 'sandbox', execution: 'prj_1', remembered: 'prod' }),
    ).toBe(sandbox);
    expect(
      pickProject({ projects, route: undefined, execution: 'prj_2', remembered: 'prod' }),
    ).toBe(sandbox);
    expect(
      pickProject({ projects, route: undefined, execution: undefined, remembered: 'sandbox' }),
    ).toBe(sandbox);
    expect(
      pickProject({ projects, route: undefined, execution: undefined, remembered: null }),
    ).toBe(prod);
  });

  it('resolves a slug or an ID, and skips a reference that no longer exists', () => {
    expect(pickProject({ projects, route: 'prj_2', execution: undefined, remembered: null })).toBe(
      sandbox,
    );
    expect(
      pickProject({ projects, route: undefined, execution: undefined, remembered: 'gone' }),
    ).toBe(prod);
    expect(
      pickProject({ projects: [], route: 'prod', execution: undefined, remembered: null }),
    ).toBe(undefined);
  });
});

describe('the remembered project', () => {
  it('is kept per organization', () => {
    rememberProject('org_1', 'sandbox');
    rememberProject('org_2', 'ops');
    expect(lastProject('org_1')).toBe('sandbox');
    expect(lastProject('org_2')).toBe('ops');
    expect(lastProject('org_3')).toBeNull();
    expect(lastProject(undefined)).toBeNull();
  });

  it('survives storage that throws', () => {
    const original = Storage.prototype.getItem;
    const originalSet = Storage.prototype.setItem;
    Storage.prototype.getItem = () => {
      throw new Error('blocked');
    };
    Storage.prototype.setItem = () => {
      throw new Error('blocked');
    };
    try {
      expect(() => rememberProject('org_1', 'sandbox')).not.toThrow();
      expect(lastProject('org_1')).toBeNull();
    } finally {
      Storage.prototype.getItem = original;
      Storage.prototype.setItem = originalSet;
    }
  });
});
