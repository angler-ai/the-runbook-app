import { defineGallery } from '../../gallery/registry';
import { Button } from '../primitives/button';
import { AppShell } from './AppShell';
import { EmptyState } from './EmptyState';
import { ErrorState } from './ErrorState';
import { PageHeader } from './PageHeader';
import { SidebarNav, type SidebarNavItem } from './SidebarNav';
import { Skeleton } from './Skeleton';
import { SplitPane } from './SplitPane';
import { BrandLockup } from './BrandMark';
import { Breadcrumbs } from './Breadcrumbs';
import { SidebarSectionLabel } from './SidebarNav';
import { BookOpen, Brain, ChevronsUpDown, Inbox, Play, Settings, Sparkles } from 'lucide-react';

const navItems: SidebarNavItem[] = [
  { id: 'runbooks', label: 'Runbooks', href: '#runbooks', icon: BookOpen, active: true },
  { id: 'executions', label: 'Runs', href: '#executions', icon: Play },
  { id: 'tasks', label: 'Inbox', href: '#tasks', icon: Inbox, count: 3 },
  { id: 'skills', label: 'Skills', href: '#', icon: Sparkles, disabled: true, note: 'Soon' },
  { id: 'memory', label: 'Memory', href: '#', icon: Brain, disabled: true, note: 'Soon' },
  { id: 'settings', label: 'Settings', href: '#', icon: Settings, disabled: true, note: 'Soon' },
];

/** The signed-in sidebar as the app composes it (features/context/AuthenticatedShell). */
function GallerySidebar() {
  return (
    <div className="flex h-full flex-col px-3">
      <div className="flex h-14 items-center px-1.5">
        <BrandLockup />
      </div>
      <div className="flex items-center gap-2.5 rounded-md border border-border bg-surface px-2 py-2">
        <span className="flex size-7 items-center justify-center rounded-md border border-accent-line bg-accent-subtle text-xs font-medium text-accent">
          PO
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-xs font-medium text-fg">Platform Ops</span>
          <span className="block truncate text-2xs text-fg-muted">Acme</span>
        </span>
        <ChevronsUpDown className="size-3.5 text-fg-muted" aria-hidden="true" />
      </div>
      <SidebarSectionLabel className="mt-5">Workspace</SidebarSectionLabel>
      <SidebarNav items={navItems} ariaLabel="Gallery navigation" />
      <div className="mt-auto flex items-center gap-2.5 border-t border-border px-1.5 py-3">
        <span className="flex size-7 items-center justify-center rounded-full border border-accent-line bg-accent-subtle text-xs font-medium text-accent">
          AK
        </span>
        <span className="text-xs font-medium text-fg">Alex Kim</span>
      </div>
    </div>
  );
}

export default [
  defineGallery({
    id: 'layout-app-shell',
    group: 'Layout',
    title: 'AppShell',
    examples: [
      {
        name: 'Full app shell',
        render: () => (
          <div className="flex h-[520px] flex-col overflow-hidden rounded-lg border border-border">
            <AppShell
              sidebar={<GallerySidebar />}
              breadcrumbs={
                <Breadcrumbs
                  items={[{ label: 'Platform Ops', href: '#' }, { label: 'Runbooks' }]}
                />
              }
              onSearchClick={() => {}}
              bottomAction={<Button className="w-full">Start run</Button>}
            >
              <div className="space-y-4">
                <PageHeader
                  eyebrow="Platform Ops"
                  title="Runbooks"
                  count="10"
                  description="Checks and procedures your agents and people run, with how each last went."
                  actions={<Button size="sm">New runbook</Button>}
                />
                <p className="text-xs text-fg-muted">Canvas content goes here.</p>
              </div>
            </AppShell>
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'layout-sidebar-nav',
    group: 'Layout',
    title: 'SidebarNav',
    examples: [
      {
        name: 'Navigation with counts',
        render: () => (
          <div className="h-[420px] w-(--rb-sidebar-width) overflow-hidden rounded-lg border border-border bg-sidebar">
            <GallerySidebar />
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'layout-page-header',
    group: 'Layout',
    title: 'PageHeader',
    examples: [
      {
        name: 'Header with actions and breadcrumbs',
        render: () => (
          <PageHeader
            breadcrumbs={
              <Breadcrumbs
                items={[
                  { label: 'core-infra', href: '#' },
                  { label: 'Runbooks', href: '#' },
                  { label: 'Database maintenance' },
                ]}
              />
            }
            title="Database maintenance"
            description="Perform primary failover and read-replica reindexing."
            actions={
              <>
                <Button variant="secondary" size="sm">
                  History
                </Button>
                <Button size="sm">Start run</Button>
              </>
            }
          />
        ),
      },
    ],
  }),
  defineGallery({
    id: 'layout-split-pane',
    group: 'Layout',
    title: 'SplitPane',
    examples: [
      {
        name: 'Two-column split',
        render: () => (
          <div className="h-64">
            <SplitPane
              left={
                <div className="space-y-2">
                  <h4 className="text-xs font-semibold text-fg">Outline</h4>
                  <p className="text-xs text-fg-muted">Left pane: step list</p>
                </div>
              }
              right={
                <div className="space-y-2">
                  <h4 className="text-xs font-semibold text-fg">Step detail</h4>
                  <p className="text-xs text-fg-muted">Right pane: checklist and outputs</p>
                </div>
              }
            />
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'layout-empty-state',
    group: 'Layout',
    title: 'EmptyState',
    examples: [
      {
        name: 'No executions',
        render: () => (
          <EmptyState
            icon={Inbox}
            title="No executions yet"
            description="Start a runbook to see active and completed executions here."
            action={<Button size="sm">Start your first run</Button>}
          />
        ),
      },
    ],
  }),
  defineGallery({
    id: 'layout-skeleton',
    group: 'Layout',
    title: 'Skeleton',
    examples: [
      {
        name: 'Content skeletons',
        render: () => (
          <div className="space-y-3 max-w-sm">
            <Skeleton className="h-6 w-3/4" />
            <Skeleton className="h-4 w-full" />
            <Skeleton className="h-4 w-5/6" />
            <div className="flex gap-2 pt-2">
              <Skeleton className="h-8 w-20" />
              <Skeleton className="h-8 w-24" />
            </div>
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'layout-error-state',
    group: 'Layout',
    title: 'ErrorState',
    examples: [
      {
        name: 'Problem error with code and requestId',
        render: () => (
          <ErrorState
            title="Failed to start execution"
            message="The provided inputs failed schema validation."
            code="execution.inputs_invalid"
            requestId="req_01HX4A7B8C9D"
            onRetry={() => {}}
          />
        ),
      },
      {
        name: 'Minimal error',
        render: () => <ErrorState message="Could not connect to server." />,
      },
    ],
  }),
];
