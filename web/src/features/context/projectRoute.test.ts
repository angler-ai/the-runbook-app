import { describe, expect, it } from 'vitest';
import { projectRoute } from './AuthenticatedShell';

describe('projectRoute', () => {
  it('sends each project nav item to its own page', () => {
    expect(projectRoute('memory')).toBe('/p/$project/memory');
    expect(projectRoute('webhooks')).toBe('/p/$project/webhooks');
    expect(projectRoute('home')).toBe('/p/$project');
    expect(projectRoute('runbooks')).toBe('/p/$project/runbooks');
    expect(projectRoute('executions')).toBe('/p/$project/executions');
    expect(projectRoute('skills')).toBe('/p/$project/skills');
  });

  it('has no route for an unknown item, rather than a wrong one', () => {
    expect(projectRoute('nope')).toBeUndefined();
  });
});
