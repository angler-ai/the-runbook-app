// Code-based route tree (M1, plan §4; UI-2): /sign-in, /home (lands on the
// current project's Home), /tasks, /p/$project (Home),
// /p/$project/runbooks[/$rb[/{editor,runs,versions}]], /p/$project/executions,
// /x/$exe. There is deliberately no /settings/tokens (§0, F90).
import {
  createRootRoute,
  createRoute,
  createRouter,
  useNavigate,
  useSearch,
  lazyRouteComponent,
  Link,
  Outlet,
  redirect,
  type RouterHistory,
} from '@tanstack/react-router';
import { registerUnauthorizedHandler } from '../api/client';
import { signedIn, useMe } from '../api/hooks';
import { useState } from 'react';
import { signOut } from '../features/auth/session';
import { isPartialMe } from '../features/mfa/hooks';
import { MfaGate } from '../features/mfa/MfaGate';
import { DeviceApprovalPage } from '../features/auth/DeviceApprovalPage';
import { AccountSettingsPage } from '../features/settings/AccountSettingsPage';
import { AccountSecurityPage } from '../features/mfa/AccountSecurityPage';
import { AuthenticatedShell } from '../features/context/AuthenticatedShell';
import { validateExecutionSearch } from '../features/executions/filters';
import { validateRunSearch } from '../features/execution/views';
import {
  ExecutionPage,
  HomeLanding,
  ProjectExecutionsPage,
  ProjectHomePage,
  ProjectRunbooksPage,
  RunbookRunsPage,
  RunbookVersionsPage,
  TasksPage,
} from './pages';
import { validateOverviewSearch } from '../features/runbook/tabs';
import { SearchPage } from '../features/search/SearchPage';
import { SignInPage } from './sign-in';
import { UnsubscribeRoute } from './unsubscribe';
import { OrganizationAdminPage } from '../features/admin/OrganizationAdminPage';
import { ProjectSettingsPage } from '../features/admin/ProjectSettingsPage';
import type { NewRunbookSearch } from '../features/builder/templates';
import { NotFound } from '../ui/pages';
import { SkillsPage } from '../features/skills/SkillsPage';
import { WebhooksPage } from '../features/webhooks/WebhooksPage';
import { ProjectMemoryPage } from '../features/memory/ProjectMemoryPage';

/**
 * An unknown URL (walkthrough #12). Signed in, it renders inside the app
 * frame (navigation, search, sign-out) with a link back to My tasks;
 * signed out, on its own with a link to sign in.
 */
function NotFoundPage() {
  const isAuthed = signedIn();
  const page = (
    <NotFound
      target={isAuthed ? undefined : { href: '/sign-in', label: 'Go to sign in' }}
      renderLink={({ href, className, children }) => (
        <Link to={href} className={className}>
          {children}
        </Link>
      )}
    />
  );
  if (isAuthed) return <AuthenticatedShell>{page}</AuthenticatedShell>;
  return <main className="p-4">{page}</main>;
}

const rootRoute = createRootRoute({
  component: Outlet,
  notFoundComponent: NotFoundPage,
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  beforeLoad: () => {
    throw redirect({ to: signedIn() ? '/home' : '/sign-in' });
  },
});

interface SignInSearch {
  reason?: string;
  next?: string;
}

const signInRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/sign-in',
  validateSearch: (search: Record<string, unknown>): SignInSearch => ({
    ...(typeof search.reason === 'string' ? { reason: search.reason } : {}),
    ...(typeof search.next === 'string' ? { next: search.next } : {}),
  }),
  beforeLoad: ({ search }) => {
    // Already signed in: skip the form unless sign-out sent us here.
    if (signedIn() && !search.reason) throw redirect({ to: '/home' });
  },
  component: SignInPage,
});

interface DeviceSearch {
  user_code?: string;
  code?: string;
}

const unsubscribeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/unsubscribe',
  validateSearch: (search: Record<string, unknown>): { token?: string } => ({
    ...(typeof search.token === 'string' ? { token: search.token } : {}),
  }),
  component: UnsubscribeRoute,
});

const deviceRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/device',
  validateSearch: (search: Record<string, unknown>): DeviceSearch => ({
    ...(typeof search.user_code === 'string' ? { user_code: search.user_code } : {}),
    ...(typeof search.code === 'string' ? { code: search.code } : {}),
  }),
  beforeLoad: ({ location }) => {
    if (!signedIn()) {
      throw redirect({
        to: '/sign-in',
        search: { next: location.pathname + location.searchStr },
      });
    }
  },
  component: DeviceRoutePage,
});

/**
 * J-3: device approval is outside the app shell, so it gates a partial
 * session itself: verify or enroll first, then approve (the approval is a
 * sensitive action, so the step-up prompt may ask again when stale).
 */
function DeviceRoutePage() {
  const me = useMe();
  const navigate = useNavigate();
  const [signingOut, setSigningOut] = useState(false);
  if (isPartialMe(me.data)) {
    return (
      <MfaGate
        security={me.data.security}
        signingOut={signingOut}
        onDone={() => void me.refetch()}
        onSignOut={() => {
          setSigningOut(true);
          void signOut().then(() =>
            navigate({ to: '/sign-in', search: { reason: 'mfa-signed-out' } }),
          );
        }}
      />
    );
  }
  return <DeviceApprovalPage />;
}

const authenticatedRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: '_authenticated',
  beforeLoad: ({ location }) => {
    if (!signedIn()) {
      throw redirect({
        to: '/sign-in',
        search: { next: location.pathname + location.searchStr },
      });
    }
  },
  component: AuthenticatedShell,
});

const searchRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/search',
  validateSearch: (search: Record<string, unknown>): { q?: string } => ({
    ...(typeof search.q === 'string' ? { q: search.q } : {}),
  }),
  component: SearchRoutePage,
});

function SearchRoutePage() {
  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as { q?: string };
  return (
    <SearchPage
      initialQuery={search.q ?? ''}
      onQueryChange={(q) => void navigate({ to: '.', search: q ? { q } : {}, replace: true })}
    />
  );
}

const tasksRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/tasks',
  component: TasksPage,
});

// UI-2: where sign-in lands. The shell knows the current project only once
// the organization's projects load, so this page forwards to its Home.
const homeLandingRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/home',
  component: HomeLanding,
});

const projectHomeRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project',
  component: ProjectHomePage,
});

const projectRunbooksRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/runbooks',
  component: ProjectRunbooksPage,
});

// UI-2: the runbook's own page, graph first (tabs in `?tab=`).
const runbookIndexRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/runbooks/$rb',
  validateSearch: validateOverviewSearch,
  // Lazy: the graph and the browser compiler load only with this page.
  component: lazyRouteComponent(() => import('./runbook'), 'RunbookOverviewRoute'),
});

// UI-4: a new runbook in the builder, not saved yet. Its first save creates it.
const newRunbookRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/runbooks/new',
  validateSearch: (search: Record<string, unknown>): NewRunbookSearch => ({
    ...(typeof search.template === 'string' ? { template: search.template } : {}),
    ...(typeof search.name === 'string' ? { name: search.name } : {}),
    ...(typeof search.id === 'string' ? { id: search.id } : {}),
  }),
  component: lazyRouteComponent(() => import('./editor'), 'NewRunbookRoute'),
});

const runbookEditorRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/runbooks/$rb/editor',
  // Lazy: Monaco, the graph and the browser compiler load only with the editor.
  component: lazyRouteComponent(() => import('./editor'), 'RunbookEditorRoute'),
});

const runbookRunsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/runbooks/$rb/runs',
  validateSearch: validateExecutionSearch,
  component: RunbookRunsPage,
});

const runbookVersionsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/runbooks/$rb/versions',
  component: RunbookVersionsPage,
});

const projectExecutionsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/executions',
  validateSearch: validateExecutionSearch,
  component: ProjectExecutionsPage,
});

const executionRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/x/$exe',
  // UI-3: the view (graph, document, steps) is in `?view=`.
  validateSearch: validateRunSearch,
  component: ExecutionPage,
});

const settingsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/settings',
  component: AccountSettingsPage,
});

// J-3: Account › Security. The auth service also sends a partial session
// here with `return_to` to resume an OAuth authorization after the check.
const securitySettingsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/settings/security',
  validateSearch: (search: Record<string, unknown>): { return_to?: string } =>
    typeof search.return_to === 'string' ? { return_to: search.return_to } : {},
  component: SecurityRoutePage,
});

function SecurityRoutePage() {
  const search = useSearch({ strict: false }) as { return_to?: string };
  return <AccountSecurityPage returnTo={search.return_to ?? null} />;
}

const organizationSettingsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/settings/organization',
  component: OrganizationAdminPage,
});

const runnerSettingsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/settings/organization/runner',
  validateSearch: (search: Record<string, unknown>): { tab?: string } =>
    typeof search.tab === 'string' ? { tab: search.tab } : {},
  component: lazyRouteComponent(
    () => import('../features/runner/RunnerSettingsPage'),
    'RunnerSettingsPage',
  ),
});

const projectSettingsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/settings',
  component: ProjectSettingsPage,
});

const agentsSearch = (search: Record<string, unknown>): { tab?: string } =>
  typeof search.tab === 'string' ? { tab: search.tab } : {};
const projectAgentsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/agents',
  validateSearch: agentsSearch,
  component: lazyRouteComponent(() => import('../features/agents/AgentsPage'), 'AgentsPage'),
});
const projectAgentRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/agents/$agent',
  validateSearch: agentsSearch,
  component: lazyRouteComponent(() => import('../features/agents/AgentsPage'), 'AgentsPage'),
});
const runnerTranscriptRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/agents/transcripts/$step',
  validateSearch: (search: Record<string, unknown>) => ({
    claim_id: typeof search.claim_id === 'string' ? search.claim_id : undefined,
  }),
  component: lazyRouteComponent(
    () => import('../features/agents/TranscriptPage'),
    'TranscriptPage',
  ),
});

const projectSkillsRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/skills',
  component: SkillsPage,
});

const projectWebhooksRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/webhooks',
  validateSearch: (search: Record<string, unknown>): { webhook?: string; new?: string } => ({
    ...(typeof search.webhook === 'string' ? { webhook: search.webhook } : {}),
    ...(typeof search.new === 'string' ? { new: search.new } : {}),
  }),
  component: WebhooksPage,
});

const projectMemoryRoute = createRoute({
  getParentRoute: () => authenticatedRoute,
  path: '/p/$project/memory',
  component: ProjectMemoryPage,
});
// Dev only: import.meta.env.DEV is statically false in production builds, so this
// branch, the gallery route and the gallery chunk are dropped from dist.
const devRoutes = import.meta.env.DEV
  ? [
      createRoute({
        getParentRoute: () => rootRoute,
        path: '/__gallery',
        component: lazyRouteComponent(() => import('../gallery/GalleryPage')),
      }),
    ]
  : [];

const routeTree = rootRoute.addChildren([
  indexRoute,
  signInRoute,
  unsubscribeRoute,
  deviceRoute,
  authenticatedRoute.addChildren([
    searchRoute,
    tasksRoute,
    homeLandingRoute,
    projectHomeRoute,
    projectRunbooksRoute,
    newRunbookRoute,
    runbookIndexRoute,
    runbookEditorRoute,
    runbookRunsRoute,
    runbookVersionsRoute,
    projectExecutionsRoute,
    executionRoute,
    settingsRoute,
    securitySettingsRoute,
    organizationSettingsRoute,
    runnerSettingsRoute,
    projectSettingsRoute,
    projectAgentsRoute,
    projectAgentRoute,
    runnerTranscriptRoute,
    projectSkillsRoute,
    projectWebhooksRoute,
    projectMemoryRoute,
  ]),
  ...devRoutes,
]);

/** `next` for the sign-in page: where the user was, unless already there. */
function nextSearch(location: { pathname: string; searchStr: string }): { next?: string } {
  return location.pathname === '/sign-in' ? {} : { next: location.pathname + location.searchStr };
}

export function createAppRouter(history?: RouterHistory) {
  const appRouter = createRouter({ routeTree, ...(history ? { history } : {}) });
  // A 401 for the current token ends the session, then lands here with
  // the page it interrupted, so signing in again returns there (U14).
  registerUnauthorizedHandler(() => {
    void appRouter.navigate({
      to: '/sign-in',
      search: { reason: 'session-expired', ...nextSearch(appRouter.state.location) },
    });
  });
  return appRouter;
}

export const router = createAppRouter();

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
