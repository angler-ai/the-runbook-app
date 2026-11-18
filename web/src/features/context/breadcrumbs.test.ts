// UI-2 review round 1: the runbook page's breadcrumb names the runbook.
import { describe, expect, it } from 'vitest';
import type { ProjectView } from '../../api/hooks';
import { shellBreadcrumbs, switchTarget } from './AuthenticatedShell';

const project = { id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' };
const runbooks = [
  { runbook_id: 'rb_1', slug: 'post-deploy', title: 'Post-deploy checks' },
  { runbook_id: 'rb_2', slug: 'untitled' },
];

describe('the shell breadcrumbs', () => {
  it("name a runbook's page by its title, by ID or slug", () => {
    expect(shellBreadcrumbs('/p/prod/runbooks/rb_1', project, runbooks)).toEqual([
      { label: 'Production', href: '/p/prod' },
      { label: 'Runbooks', href: '/p/prod/runbooks' },
      { label: 'Post-deploy checks' },
    ]);
    expect(shellBreadcrumbs('/p/prod/runbooks/post-deploy', project, runbooks).at(-1)).toEqual({
      label: 'Post-deploy checks',
    });
    expect(shellBreadcrumbs('/p/prod/runbooks/rb_2', project, runbooks).at(-1)).toEqual({
      label: 'untitled',
    });
  });

  it("put the runbook before its sub-page, linked to the runbook's page", () => {
    expect(shellBreadcrumbs('/p/prod/runbooks/rb_1/editor', project, runbooks).slice(2)).toEqual([
      { label: 'Post-deploy checks', href: '/p/prod/runbooks/rb_1' },
      { label: 'Editor' },
    ]);
  });

  it('fall back to "Runbook" while the catalog is unknown', () => {
    expect(shellBreadcrumbs('/p/prod/runbooks/rb_3', project, []).at(-1)).toEqual({
      label: 'Runbook',
    });
  });

  it('name the new-runbook page (UI-4)', () => {
    expect(shellBreadcrumbs('/p/prod/runbooks/new', project, runbooks).slice(1)).toEqual([
      { label: 'Runbooks', href: '/p/prod/runbooks' },
      { label: 'New runbook' },
    ]);
  });
  it('name organization and project settings pages', () => {
    expect(shellBreadcrumbs('/settings/organization', project, runbooks)).toEqual([
      { label: 'Settings' },
      { label: 'Organization' },
    ]);
    expect(shellBreadcrumbs('/p/prod/settings', project, runbooks)).toEqual([
      { label: 'Production', href: '/p/prod' },
      { label: 'Settings' },
    ]);
  });

  it('name the skills and webhooks pages', () => {
    expect(shellBreadcrumbs('/p/prod/skills', project, runbooks)).toEqual([
      { label: 'Production', href: '/p/prod' },
      { label: 'Skills' },
    ]);
    expect(shellBreadcrumbs('/p/prod/webhooks', project, runbooks)).toEqual([
      { label: 'Production', href: '/p/prod' },
      { label: 'Webhooks' },
    ]);
  });

  it('switches targets across projects for skills and webhooks', () => {
    expect(switchTarget('/p/prod/skills', 'prod', 'staging')).toBe('/p/staging/skills');
    expect(switchTarget('/p/prod/webhooks', 'prod', 'staging')).toBe('/p/staging/webhooks');
  });
});

const pilot = { id: 'prj_1', slug: 'pilot', title: 'Pilot' } as ProjectView;
const catalog = [{ runbook_id: 'rb_1', slug: 'onboarding-demo', title: 'Customer onboarding' }];

describe('run page breadcrumbs', () => {
  it('name the project, the runbook and the run, linking all but the run', () => {
    expect(
      shellBreadcrumbs('/x/exe_1', pilot, catalog, {
        title: 'Customer onboarding',
        runbook_id: 'rb_1',
        runbook_slug: 'onboarding-demo',
        summary: 'customer=Northwind Goods',
      }),
    ).toEqual([
      { label: 'Pilot', href: '/p/pilot' },
      { label: 'Runbooks', href: '/p/pilot/runbooks' },
      { label: 'Customer onboarding', href: '/p/pilot/runbooks/rb_1' },
      { label: 'Run · customer=Northwind Goods' },
    ]);
  });

  it("use the run's own title when it differs from the runbook's", () => {
    const crumbs = shellBreadcrumbs('/x/exe_1', pilot, catalog, {
      title: 'Verify api v2.0.1',
      runbook_id: 'rb_1',
    });
    expect(crumbs.at(-1)).toEqual({ label: 'Verify api v2.0.1' });
  });

  it('fall back to Runs › Run while the run loads', () => {
    expect(shellBreadcrumbs('/x/exe_1', pilot, catalog)).toEqual([
      { label: 'Pilot', href: '/p/pilot' },
      { label: 'Runs', href: '/p/pilot/executions' },
      { label: 'Run' },
    ]);
  });
});
