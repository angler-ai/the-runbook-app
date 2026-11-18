import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { server } from '../../test/msw/server';

const versions = [
  {
    runbook_id: 'rb_1',
    runbook_version_id: 'rbv_2',
    runbook_version: 2,
    branch: null,
    commit_sha: null,
  },
  {
    runbook_id: 'rb_1',
    runbook_version_id: 'rbv_1',
    runbook_version: 1,
    branch: null,
    commit_sha: null,
  },
];

function handlers() {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'owner' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes: ['runbooks:read', 'executions:read'],
          projects: null,
          expires_at_ms: Date.now() + 3_600_000,
        },
      }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [{ id: 'prj_1', org_id: 'org_1', slug: 'prod', title: 'Production' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/me/tasks', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/projects/:p/executions', () =>
      HttpResponse.json({ items: [], next_cursor: null }),
    ),
    http.get('*/v1/runbooks/:rb/versions', () =>
      HttpResponse.json({ items: versions, next_cursor: null }),
    ),
    http.get('*/v1/runbooks/:rb/versions/:a/diff/:b', () =>
      HttpResponse.json({
        unified_diff: '--- 1\n+++ 2\n@@ -1 +1 @@\n-title: Old\n-title: New\n+title: New',
        added_steps: ['notify'],
        removed_steps: [],
        changed_steps: ['smoke'],
      }),
    ),
    http.get('*/v1/runbooks/:rb/versions/:version', ({ params }) =>
      HttpResponse.json({
        runbook_id: 'rb_1',
        runbook_version_id: params.version === '1' ? 'rbv_1' : 'rbv_2',
        runbook_version: Number(params.version),
        branch: null,
        commit_sha: null,
        source: `title: ${params.version === '1' ? 'Old' : 'New'}`,
        semantic_hash: 'ab',
        snapshot_digest: 'cd',
      }),
    ),
  ];
}

afterEach(() => tokenStore.clear());

describe('versions and diff', () => {
  it('diffs any two selected versions with both sources loaded', async () => {
    server.use(...handlers());
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(
      createMemoryHistory({ initialEntries: ['/p/prod/runbooks/rb_1/versions'] }),
    );
    render(<App router={router} />);
    const user = userEvent.setup();
    expect(await screen.findByLabelText(/compare from/i)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /compare versions/i }));
    expect(await screen.findByText(/changes from 1 to 2/i)).toBeInTheDocument();
    expect(await screen.findByText(/smoke/)).toBeInTheDocument();
    expect(screen.getByText(/notify/)).toBeInTheDocument();
  });
});
