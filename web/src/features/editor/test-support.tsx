// Shared fixtures for the editor tests (M5): MSW handlers for one runbook's
// draft, catalog row and compile directory, and a rendering harness. The YAML
// view is a textarea stand-in (Monaco needs a real browser; e2e covers it),
// mocked in each test file with `vi.mock('./YamlEditor', …)`.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from '@tanstack/react-router';
import { render } from '@testing-library/react';
import { http, HttpResponse, type JsonBodyType } from 'msw';
import { OrgProvider } from '../context/OrgContext';
import type { CompilerBackend } from '../compile/useCompiler';
import type { CompileRequest, Diagnostic } from '../../wasm/runbook';
import type { EditorMarker, YamlEditorProps } from './YamlEditor';
import { RunbookEditorPage } from './RunbookEditorPage';

export const RB = 'rb_01ARZ3NDEKTSV4RRFFQ69G5FAV';
export const etagOf = (revision: number) => `"draft-${RB}-${revision}"`;

export const EDITOR_SCOPES = [
  'runbooks:read',
  'runbooks:write',
  'runbooks:publish',
  'skills:read',
  'executions:read',
];

export function problem(status: number, code: string, errors: JsonBodyType[] = []) {
  return HttpResponse.json(
    {
      type: 'about:blank',
      title: code,
      status,
      detail: code,
      instance: 'urn:request:req_test',
      code,
      errors,
    },
    { status, headers: { 'Content-Type': 'application/problem+json' } },
  );
}

export interface EditorServer {
  draft: { source: string; revision: number };
  latestVersion: number | null;
  origin?: 'ui' | 'git';
  scopes?: string[];
  teams?: { id: string; slug: string }[];
  skills?: { name: string; hash: string; reference?: string | null }[] | 'forbidden';
  /** UI-4: the project's runbook list (call targets). */
  catalog?: {
    runbook_id: string;
    slug: string;
    title: string;
    origin: 'ui' | 'git';
    archived: boolean;
    latest_version: number | null;
    tags: string[];
    draft_ahead: boolean;
  }[];
}

/** Handlers for the shell, the directory and one runbook (draft PATCH/publish are per test). */
export function editorHandlers(state: EditorServer) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_01ARZ3NDEKTSV4RRFFQ69G5FAA', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'member' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes: state.scopes ?? EDITOR_SCOPES,
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
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({
        items: [
          { id: 'usr_01ARZ3NDEKTSV4RRFFQ69G5FAA', name: 'Alice', active: true, role: 'member' },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/teams', () =>
      HttpResponse.json({ items: state.teams ?? [], next_cursor: null }),
    ),
    http.get('*/v1/projects/:p/skills', () =>
      state.skills === 'forbidden'
        ? problem(403, 'auth.scope_missing')
        : HttpResponse.json({
            items: (state.skills ?? []).map((s) => ({
              ...s,
              description: '',
              files: [],
              usages: [],
            })),
            next_cursor: null,
          }),
    ),
    http.get('*/v1/projects/:p/vars', () => HttpResponse.json({ declarations: {}, values: {} })),
    // UI-4: the call picker lists the project's published runbooks.
    http.get('*/v1/projects/:p/runbooks', () =>
      HttpResponse.json({ items: state.catalog ?? [], next_cursor: null }),
    ),
    // UI-1c: one runbook by its slug in the project.
    http.get('*/v1/projects/:p/runbooks/:slug', ({ params }) => {
      const row = (state.catalog ?? []).find((r) => r.slug === params.slug);
      return row ? HttpResponse.json(row) : problem(404, 'resource.not_found');
    }),
    http.get('*/v1/runbooks/:rb/draft', () =>
      HttpResponse.json(
        { runbook_id: RB, source: state.draft.source, revision: state.draft.revision },
        { headers: { ETag: etagOf(state.draft.revision) } },
      ),
    ),
    http.get('*/v1/runbooks/:rb', () =>
      HttpResponse.json({
        runbook_id: RB,
        slug: 'post-deploy',
        origin: state.origin ?? 'ui',
        archived: false,
        title: 'Post-deploy verification',
        latest_version: state.latestVersion,
      }),
    ),
  ];
}

/** A compiler stand-in: every validate reports `diagnostics(source)`. */
export function fakeCompiler(
  diagnostics: (request: CompileRequest) => Diagnostic[],
): CompilerBackend {
  return {
    validate: async (request) => ({ diagnostics: diagnostics(request), step_budget: 7 }),
    outline: async () => ({
      outline: {
        id: 'post-deploy',
        title: 'Post-deploy verification',
        steps: [],
        finally: [],
        stepBudget: null,
      },
      diagnostics: [],
    }),
    describe: async () => ({ runbook: null, diagnostics: [] }),
  };
}

/** The editor at its route, in a memory router (call links navigate in the app). */
/** The editor on a memory router; `isNew` opens the new-runbook page instead. */
export function renderEditor(compiler: CompilerBackend, { isNew = false } = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: 0 } },
  });
  const root = createRootRoute({
    component: () => (
      <main>
        <Outlet />
      </main>
    ),
  });
  const editor = createRoute({
    getParentRoute: () => root,
    path: '/p/$p/runbooks/$rb/edit',
    component: () => <RunbookEditorPage projectRef="prod" runbookId={RB} compiler={compiler} />,
  });
  const created = createRoute({
    getParentRoute: () => root,
    path: '/p/$p/runbooks/new',
    component: () => <RunbookEditorPage projectRef="prod" compiler={compiler} />,
  });
  const list = createRoute({
    getParentRoute: () => root,
    path: '/p/$p/runbooks',
    component: () => <p>Runbooks list</p>,
  });
  const runbook = createRoute({
    getParentRoute: () => root,
    path: '/p/$p/runbooks/$rb',
    component: () => <p>Runbook page</p>,
  });
  const router = createRouter({
    routeTree: root.addChildren([editor, created, runbook, list]),
    history: createMemoryHistory({
      initialEntries: [isNew ? '/p/prod/runbooks/new' : `/p/prod/runbooks/${RB}/edit`],
    }),
  });
  const utils = render(
    <QueryClientProvider client={client}>
      <OrgProvider>
        <RouterProvider router={router} />
      </OrgProvider>
    </QueryClientProvider>,
  );
  return { router, ...utils };
}

/** The textarea stand-in for Monaco: same props, markers listed as text. */
export function StubYamlEditor({
  value,
  onChange,
  markers,
  readOnly,
  ariaLabel,
}: Omit<YamlEditorProps, 'resetKey'>) {
  return (
    <div>
      <textarea
        aria-label={ariaLabel}
        value={value}
        readOnly={readOnly}
        onChange={(event) => onChange(event.target.value)}
      />
      <ul aria-label="Editor markers">
        {markers.map((m: EditorMarker, i) => (
          <li
            key={i}
          >{`${m.provisional ? 'provisional' : 'server'} ${m.line}:${m.column} ${m.code}`}</li>
        ))}
      </ul>
    </div>
  );
}
