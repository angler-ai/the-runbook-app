// Authenticated shell wiring: the A3 AppShell/SidebarNav with
// the real identity, org/project context, inbox count and sign-out.
import {
  Link,
  Outlet,
  useNavigate,
  useParams,
  useRouter,
  useRouterState,
} from '@tanstack/react-router';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useState, type ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { endSession, onSessionEnd } from '../../api/session-end';
import { tokenStore } from '../../api/token-store';
import { useExecution, useExecutionDocument, useMe, useRunbooks } from '../../api/hooks';
import { dataSummary } from '../execution/runIdentity';
import { useExecutionSearch } from '../executions/hooks';
import { useInbox } from '../inbox/hooks';
import { PaletteHost } from '../palette/PaletteHost';
import { CreateProjectDialog } from '../admin/CreateProjectDialog';
import { StartRunDialog } from '../start/StartRunDialog';
import { BookOpen, Brain, House, Inbox, Play, Settings, Sparkles, Webhook } from 'lucide-react';
import {
  AppShell,
  BrandLockup,
  BrandMark,
  type RenderLinkProps,
  Breadcrumbs,
  ErrorState,
  SidebarNav,
  SidebarSectionLabel,
  Skeleton,
  type BreadcrumbItem,
  type SidebarNavItem,
} from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import type { ProjectView } from '../../api/hooks';
import { ProjectSwitcher, UserMenu } from './ShellMenus';
import { WelcomeDialog } from '../profile/WelcomeDialog';
import { signOut, validateMeForSignIn } from '../auth/session';
import { OrgProvider, useOrgContext } from './OrgContext';
import { pickProject, rememberProject, useLastProject } from './currentProject';

function ShellChrome({ children }: { children?: ReactNode }) {
  const me = useMe();
  const tasks = useInbox();
  const { org, projects } = useOrgContext();
  const params = useParams({ strict: false }) as { project?: string; exe?: string };
  const orgId = org?.id ?? me.data?.org.id;
  // UI-2: the project never falls back to the first one on pages without
  // `$project` in their URL. A run's page uses the run's own project (the
  // same query the page reads), anything else the last project chosen.
  const executionDocument = useExecutionDocument(params.exe);
  const remembered = useLastProject(orgId);
  const current = pickProject({
    projects,
    route: params.project,
    execution: params.exe ? executionDocument.data?.header.project_id : undefined,
    remembered,
  });
  const fromPage =
    projects.length > 0 &&
    (Boolean(params.project && current && [current.id, current.slug].includes(params.project)) ||
      Boolean(params.exe && current && executionDocument.data?.header.project_id === current.id));
  useEffect(() => {
    if (fromPage && current) rememberProject(orgId, current.slug);
  }, [fromPage, current, orgId]);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  // A run page's breadcrumbs name its runbook and the run (cached with the
  // run page's own read of the run).
  const runId = pathname.startsWith('/x/') ? pathname.split('/')[2] : undefined;
  const runForCrumbs = useExecution(runId);
  const navigate = useNavigate();
  const router = useRouter();
  const queryClient = useQueryClient();
  // Every way out (sign-out, a 401, the refusal below) drops the cache, so
  // the next person never renders from the last one's `me`, inbox or runs.
  useEffect(() => onSessionEnd(() => queryClient.clear()), [queryClient]);
  const [signingOut, setSigningOut] = useState(false);
  const [startOpen, setStartOpen] = useState(false);
  const [createProjectOpen, setCreateProjectOpen] = useState(false);
  const paletteRunbooks = useRunbooks(current?.id);
  const paletteExecutions = useExecutionSearch(current?.id, {
    statuses: [],
    runbook: '',
    tags: [],
  });

  // A restored token must still qualify (F90): it may have gained wide
  // scopes or expired since it was stored in this tab. Cookie sessions carry full scopes.
  const [checkedAt] = useState(() => Date.now());
  const refusal = me.data && tokenStore.get() ? validateMeForSignIn(me.data, checkedAt) : null;
  const refusalKind = refusal?.kind;
  useEffect(() => {
    if (refusalKind) {
      const { pathname, searchStr } = router.state.location;
      endSession();
      void navigate({
        to: '/sign-in',
        search: { reason: 'session-expired', next: pathname + searchStr },
      });
    }
  }, [refusalKind, navigate, router]);

  async function onSignOut() {
    setSigningOut(true);
    const { revoked } = await signOut();
    await navigate({
      to: '/sign-in',
      search: { reason: revoked ? 'signed-out' : 'revoke-failed' },
    });
  }

  if (refusal) {
    return (
      <main className="p-4">
        <p role="status">That session no longer qualifies for the web app. Signing you out…</p>
      </main>
    );
  }
  if (me.isLoading) {
    return (
      <main className="grid gap-3 p-4" aria-busy="true">
        <p role="status" className="sr-only">
          Loading your session…
        </p>
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-32 w-full max-w-xl" />
      </main>
    );
  }
  if (me.isError) {
    const error = me.error instanceof ApiError ? me.error : null;
    return (
      <main className="grid gap-4 p-4">
        <ErrorState
          title="Could not load your session"
          message="The session check failed. Retry, or sign out and try another token."
          code={error?.code}
          requestId={error?.requestId ?? undefined}
          onRetry={() => void me.refetch()}
        />
        <p className="text-center">
          <Button variant="secondary" disabled={signingOut} onClick={() => void onSignOut()}>
            Sign out and try another token
          </Button>
        </p>
      </main>
    );
  }

  const inboxCount = tasks.data
    ? // Past the paging cap the count is a lower bound: "1000+" (U20).
      tasks.data.truncated
      ? `${tasks.data.items.length}+`
      : tasks.data.items.length
    : undefined;
  const home = current ? `/p/${current.slug}` : null;
  const items: SidebarNavItem[] = [
    ...(current
      ? [
          {
            id: 'home',
            label: 'Home',
            href: `/p/${current.slug}`,
            icon: House,
            active: pathname === home || pathname === `${home}/`,
          },
          {
            id: 'runbooks',
            label: 'Runbooks',
            href: `/p/${current.slug}/runbooks`,
            icon: BookOpen,
            active: pathname.startsWith(`/p/${current.slug}/runbooks`),
          },
          {
            id: 'executions',
            label: 'Runs',
            href: `/p/${current.slug}/executions`,
            icon: Play,
            active:
              pathname.startsWith(`/p/${current.slug}/executions`) || pathname.startsWith('/x/'),
          },
          {
            id: 'skills',
            label: 'Skills',
            href: `/p/${current.slug}/skills`,
            icon: Sparkles,
            active: pathname.startsWith(`/p/${current.slug}/skills`),
          },
          {
            id: 'webhooks',
            label: 'Webhooks',
            href: `/p/${current.slug}/webhooks`,
            icon: Webhook,
            active: pathname.startsWith(`/p/${current.slug}/webhooks`),
          },
          {
            id: 'memory',
            label: 'Memory',
            href: `/p/${current.slug}/memory`,
            icon: Brain,
            active: pathname.startsWith(`/p/${current.slug}/memory`),
          },
        ]
      : []),
    {
      id: 'tasks',
      label: 'Inbox',
      href: '/tasks',
      icon: Inbox,
      active: pathname === '/tasks',
      ...(inboxCount !== undefined ? { count: inboxCount } : {}),
    },
    {
      id: 'settings',
      label: 'Settings',
      href: '/settings',
      icon: Settings,
      active: pathname === '/settings' || pathname.startsWith('/settings/'),
    },
  ];

  const renderNavLink = ({ item, className, children }: RenderLinkProps) => {
    if (item.id === 'tasks') {
      return (
        <Link to="/tasks" className={className} aria-current={item.active ? 'page' : undefined}>
          {children}
        </Link>
      );
    }
    if (item.id === 'settings') {
      return (
        <Link to="/settings" className={className} aria-current={item.active ? 'page' : undefined}>
          {children}
        </Link>
      );
    }
    if (!current) return null;
    const to = projectRoute(item.id);
    if (!to) return null;
    return (
      <Link
        to={to}
        params={{ project: current.slug }}
        className={className}
        aria-current={item.active ? 'page' : undefined}
      >
        {children}
      </Link>
    );
  };

  const orgTitle = org?.title ?? me.data?.org.title ?? '';
  const sidebar = (
    <div className="flex h-full flex-col px-3">
      <div className="flex h-14 shrink-0 items-center px-1.5">
        {current ? (
          <Link
            to="/p/$project"
            params={{ project: current.slug }}
            className="rounded-md"
            aria-label="The Runbook: home"
          >
            <BrandLockup />
          </Link>
        ) : (
          <Link to="/tasks" className="rounded-md" aria-label="The Runbook: inbox">
            <BrandLockup />
          </Link>
        )}
      </div>
      <ProjectSwitcher
        orgTitle={orgTitle}
        current={current}
        projects={projects}
        onSelect={(slug) => {
          rememberProject(orgId, slug);
          const to = switchTarget(pathname, current?.slug, slug);
          if (to) void navigate({ to });
        }}
        onNewProject={() => setCreateProjectOpen(true)}
      />
      <SidebarSectionLabel className="mt-5">Workspace</SidebarSectionLabel>
      <SidebarNav ariaLabel="Primary" items={items} renderLink={renderNavLink} />

      <div className="mt-auto border-t border-border py-3">
        <UserMenu
          userId={me.data?.user?.id}
          name={me.data?.user?.name ?? 'Signed in'}
          role={me.data?.org.role ?? undefined}
          signingOut={signingOut}
          onSignOut={() => void onSignOut()}
        />
      </div>
    </div>
  );

  // The collapsed sidebar, from lg up: the mark, the navigation as icons and
  // the account, each named for screen readers and in a tooltip.
  const rail = (
    <div className="flex h-full flex-col items-center gap-1 py-2.5">
      <Link
        to={current ? '/p/$project' : '/tasks'}
        {...(current ? { params: { project: current.slug } } : {})}
        className="mb-3 flex size-9 items-center justify-center rounded-md"
        aria-label={current ? 'The Runbook: home' : 'The Runbook: inbox'}
        title="The Runbook"
      >
        <BrandMark />
      </Link>
      <SidebarNav compact ariaLabel="Primary" items={items} renderLink={renderNavLink} />
      <div className="mt-auto pt-2">
        <UserMenu
          userId={me.data?.user?.id}
          compact
          name={me.data?.user?.name ?? 'Signed in'}
          role={me.data?.org.role ?? undefined}
          signingOut={signingOut}
          onSignOut={() => void onSignOut()}
        />
      </div>
    </div>
  );

  const crumbRun: CrumbRun | undefined = runForCrumbs.data
    ? {
        title: runForCrumbs.data.title,
        runbook_id: runForCrumbs.data.runbook_id,
        runbook_slug: runForCrumbs.data.runbook_slug,
        summary: dataSummary(runForCrumbs.data.inputs) ?? dataSummary(runForCrumbs.data.fields),
      }
    : undefined;

  return (
    <>
      <WelcomeDialog />
      <AppShell
        sidebar={sidebar}
        rail={rail}
        breadcrumbs={
          <Breadcrumbs
            items={shellBreadcrumbs(pathname, current, paletteRunbooks.data, crumbRun)}
            renderLink={({ href, className, children }) => (
              <Link to={href} className={className}>
                {children}
              </Link>
            )}
          />
        }
        searchTrigger={
          <PaletteHost
            projectRef={current?.slug}
            runbooks={paletteRunbooks.data ?? []}
            executions={(paletteExecutions.data?.pages[0]?.items ?? []).map((e) => ({
              id: e.id,
              title: e.title,
            }))}
            onStartRun={() => setStartOpen(true)}
          />
        }
      >
        {children ?? <Outlet />}
        {current ? (
          <StartRunDialog
            open={startOpen}
            onOpenChange={setStartOpen}
            projectId={current.id}
            projectRef={current.slug}
            runbooks={paletteRunbooks.data ?? []}
          />
        ) : null}
        <CreateProjectDialog open={createProjectOpen} onOpenChange={setCreateProjectOpen} />
      </AppShell>
    </>
  );
}

/**
 * Where the project switcher goes: the same section in the chosen project
 * (Home for pages that belong to one runbook or run), or nowhere on pages
 * outside any project (/tasks), which follow the remembered project.
 */
export function switchTarget(
  pathname: string,
  from: string | undefined,
  to: string,
): string | null {
  if (pathname === '/tasks') return null;
  const base = from ? `/p/${from}` : null;
  if (base && pathname.startsWith(`${base}/runbooks`)) return `/p/${to}/runbooks`;
  if (base && pathname.startsWith(`${base}/executions`)) return `/p/${to}/executions`;
  if (base && pathname.startsWith(`${base}/skills`)) return `/p/${to}/skills`;
  if (base && pathname.startsWith(`${base}/webhooks`)) return `/p/${to}/webhooks`;
  if (base && pathname.startsWith(`${base}/settings`)) return `/p/${to}/settings`;
  return `/p/${to}`;
}

/** A catalog row as the breadcrumbs read it. */
interface CrumbRunbook {
  runbook_id: string;
  slug: string;
  title?: string | null;
}

/** A run as its page's breadcrumbs read it. */
export interface CrumbRun {
  title: string;
  runbook_id?: string | null;
  runbook_slug?: string | null;
  /** What tells this run apart ("customer=Northwind Goods"), if anything. */
  summary?: string;
}

/**
 * Where this page sits: project, section, and the page when it is deeper.
 * A runbook's pages name the runbook by its title (from the project's
 * catalog, by ID or slug), or "Runbook" until the catalog loads.
 */
export function shellBreadcrumbs(
  pathname: string,
  project: ProjectView | undefined,
  runbooks: readonly CrumbRunbook[] = [],
  run?: CrumbRun,
): BreadcrumbItem[] {
  if (pathname === '/tasks') return [{ label: 'Inbox' }];
  if (pathname === '/search') {
    return project
      ? [{ label: project.title, href: `/p/${project.slug}` }, { label: 'Search' }]
      : [{ label: 'Search' }];
  }
  if (pathname.startsWith('/settings/organization')) {
    return [{ label: 'Settings' }, { label: 'Organization' }];
  }
  if (pathname === '/settings' || pathname.startsWith('/settings/')) {
    return [{ label: 'Account settings' }];
  }
  if (pathname.startsWith('/x/')) {
    // Project › Runbooks › the runbook › this run, each but the last linked.
    if (!project) return [{ label: run?.title ?? 'Run' }];
    const base = `/p/${project.slug}`;
    const row = run
      ? runbooks.find((r) => r.runbook_id === run.runbook_id || r.slug === run.runbook_slug)
      : undefined;
    const runbookName = row ? row.title || row.slug : run?.runbook_slug;
    const runLabel = !run
      ? 'Run'
      : run.title !== runbookName
        ? run.title
        : run.summary
          ? `Run · ${run.summary}`
          : 'Run';
    return [
      { label: project.title, href: base },
      ...(runbookName
        ? [
            { label: 'Runbooks', href: `${base}/runbooks` },
            {
              label: runbookName,
              href: `${base}/runbooks/${row?.runbook_id ?? run?.runbook_slug}`,
            },
          ]
        : [{ label: 'Runs', href: `${base}/executions` }]),
      { label: runLabel },
    ];
  }
  if (!project) return [{ label: 'The Runbook' }];
  const base = `/p/${project.slug}`;
  if (pathname === base || pathname === `${base}/`)
    return [{ label: project.title }, { label: 'Home' }];
  const crumbs: BreadcrumbItem[] = [{ label: project.title, href: base }];
  if (pathname.startsWith(`${base}/runbooks/`)) {
    crumbs.push({ label: 'Runbooks', href: `${base}/runbooks` });
    const [ref, page] = pathname.split('/').slice(4);
    // UI-4: the builder's new runbook (the route wins over a runbook slug `new`).
    if (ref === 'new' && !page) {
      crumbs.push({ label: 'New runbook' });
      return crumbs;
    }
    const row = runbooks.find((r) => r.runbook_id === ref || r.slug === ref);
    const name = row ? row.title || row.slug : 'Runbook';
    const sub =
      page === 'runs'
        ? 'Runs'
        : page === 'versions'
          ? 'Versions'
          : page === 'editor'
            ? 'Editor'
            : undefined;
    if (sub) {
      crumbs.push({ label: name, href: `${base}/runbooks/${ref}` });
      crumbs.push({ label: sub });
    } else {
      crumbs.push({ label: name });
    }
  } else if (pathname.startsWith(`${base}/runbooks`)) {
    crumbs.push({ label: 'Runbooks' });
  } else if (pathname.startsWith(`${base}/executions`)) {
    crumbs.push({ label: 'Runs' });
  } else if (pathname.startsWith(`${base}/settings`)) {
    crumbs.push({ label: 'Settings' });
  } else if (pathname.startsWith(`${base}/skills`)) {
    crumbs.push({ label: 'Skills' });
  } else if (pathname.startsWith(`${base}/webhooks`)) {
    crumbs.push({ label: 'Webhooks' });
  } else if (pathname.startsWith(`${base}/memory`)) {
    crumbs.push({ label: 'Memory' });
  }
  return crumbs;
}

/**
 * Authenticated layout: org context plus the A3 shell. Its page is the
 * route's outlet, or `children` for a page outside the route tree (the
 * signed-in "Page not found", walkthrough #12).
 */
/** Each project nav item's route. An item missing here renders no link
 * (it used to fall through to Webhooks, which sent Memory there). */
const PROJECT_ROUTES = {
  home: '/p/$project',
  runbooks: '/p/$project/runbooks',
  executions: '/p/$project/executions',
  skills: '/p/$project/skills',
  webhooks: '/p/$project/webhooks',
  memory: '/p/$project/memory',
} as const;

export function projectRoute(id: string) {
  return (PROJECT_ROUTES as Record<string, (typeof PROJECT_ROUTES)[keyof typeof PROJECT_ROUTES]>)[
    id
  ];
}

export function AuthenticatedShell({ children }: { children?: ReactNode }) {
  return (
    <OrgProvider>
      <ShellChrome>{children}</ShellChrome>
    </OrgProvider>
  );
}
