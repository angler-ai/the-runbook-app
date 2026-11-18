// UI-1: the app frame in Shelf's layout: a 232px tinted sidebar (brand,
// project, nav, account at the foot), a quiet 56px header with breadcrumbs
// and search, and the page on a white canvas. Below `lg` the sidebar is a
// drawer; from `lg` up it can be collapsed (remembered in this browser) to a
// narrow rail of icons.
import { useEffect, useId, useState, type ReactNode } from 'react';
import { Menu, PanelLeftClose, PanelLeftOpen, Search } from 'lucide-react';
import { cn } from '../primitives/cn';
import { Sheet, SheetContent, SheetHeader, SheetTitle } from '../primitives/sheet';

export interface AppShellProps {
  /** Sidebar content: brand, project switcher, navigation, account. */
  sidebar: ReactNode;
  /** From `lg` up, what a collapsed sidebar leaves: a narrow rail of icons. */
  rail?: ReactNode;
  /** Breadcrumb slot in the header. */
  breadcrumbs?: ReactNode;
  /** Custom ⌘K trigger component or button. */
  searchTrigger?: ReactNode;
  /** Click handler for the default ⌘K search button. */
  onSearchClick?: () => void;
  /** Additional actions on the right side of the header. */
  topBarActions?: ReactNode;
  /** Bottom action slot anchored at <= 640px. */
  bottomAction?: ReactNode;
  /** Main canvas content. */
  children: ReactNode;
  className?: string;
}

const COLLAPSED_KEY = 'rb.sidebar-collapsed';

function readCollapsed(): boolean {
  try {
    return window.localStorage.getItem(COLLAPSED_KEY) === '1';
  } catch {
    return false;
  }
}

/** Shelf's search trigger: quiet, with the shortcut in a mono key cap. */
export function SearchTrigger({
  onClick,
  className,
}: {
  onClick?: () => void;
  className?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      data-slot="command-palette-trigger"
      className={cn(
        'inline-flex h-8 items-center gap-2 rounded-md border border-border bg-surface px-2.5 text-xs text-fg-muted transition-colors duration-[120ms] select-none hover:border-border-strong hover:text-fg md:w-56',
        className,
      )}
      aria-label="Search ⌘K (open command palette)"
    >
      <Search className="size-3.5 shrink-0" aria-hidden="true" />
      <span className="hidden md:inline">Search</span>
      <kbd className="ml-auto hidden rounded border border-border px-1.5 py-0.5 font-mono text-2xs leading-none text-fg-muted sm:inline">
        ⌘K
      </kbd>
    </button>
  );
}

export function AppShell({
  sidebar,
  rail,
  breadcrumbs,
  searchTrigger,
  onSearchClick,
  topBarActions,
  bottomAction,
  children,
  className,
}: AppShellProps) {
  const [mobileDrawerOpen, setMobileDrawerOpen] = useState(false);
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const sidebarId = useId();

  useEffect(() => {
    try {
      window.localStorage.setItem(COLLAPSED_KEY, collapsed ? '1' : '0');
    } catch {
      // Not remembered; the toggle still works for this page.
    }
  }, [collapsed]);

  return (
    <div data-slot="app-shell" className={cn('flex min-h-dvh bg-canvas text-fg', className)}>
      {/* Desktop sidebar (232px, Shelf) */}
      <aside
        id={sidebarId}
        data-slot="app-shell-sidebar"
        className={cn(
          'sticky top-0 z-20 hidden h-dvh w-(--rb-sidebar-width) shrink-0 flex-col overflow-y-auto border-r border-border bg-sidebar',
          collapsed ? 'lg:hidden' : 'lg:flex',
        )}
      >
        {sidebar}
      </aside>
      {/* Collapsed, from lg up: the rail keeps the navigation one click away. */}
      {rail && collapsed ? (
        <aside
          data-slot="app-shell-rail"
          aria-label="Navigation"
          className="sticky top-0 z-20 hidden h-dvh w-14 shrink-0 flex-col overflow-y-auto border-r border-border bg-sidebar lg:flex"
        >
          {rail}
        </aside>
      ) : null}

      {/* Mobile drawer (below lg) */}
      <Sheet open={mobileDrawerOpen} onOpenChange={setMobileDrawerOpen}>
        <SheetContent
          side="left"
          className="w-[264px] max-w-[85%] gap-0 bg-sidebar p-0 lg:hidden"
          // Following a link navigates (client-side, so nothing reloads): the
          // drawer must not stay open over the new page (U20).
          onClick={(event) => {
            if (event.target instanceof Element && event.target.closest('a[href]')) {
              setMobileDrawerOpen(false);
            }
          }}
        >
          <SheetHeader className="sr-only">
            <SheetTitle>Navigation Menu</SheetTitle>
          </SheetHeader>
          <div className="flex h-full flex-col overflow-y-auto">{sidebar}</div>
        </SheetContent>
      </Sheet>

      {/* Main column */}
      <div className="flex min-w-0 flex-1 flex-col">
        <header
          data-slot="app-shell-topbar"
          className="sticky top-0 z-10 flex h-(--rb-header-height) items-center justify-between gap-3 border-b border-border bg-canvas px-4 lg:px-6"
        >
          <div className="flex min-w-0 items-center gap-2">
            <button
              type="button"
              className="-ml-1.5 inline-flex size-11 items-center justify-center rounded-md text-fg-muted hover:bg-hover hover:text-fg sm:size-9 lg:hidden"
              onClick={() => setMobileDrawerOpen(true)}
              aria-label="Open navigation menu"
            >
              <Menu className="size-4" aria-hidden="true" />
            </button>
            <button
              type="button"
              data-slot="sidebar-collapse-toggle"
              className="-ml-1.5 hidden size-8 items-center justify-center rounded-md text-fg-muted hover:bg-hover hover:text-fg lg:inline-flex"
              onClick={() => setCollapsed((value) => !value)}
              aria-label={collapsed ? 'Show sidebar' : 'Hide sidebar'}
              aria-controls={sidebarId}
              aria-expanded={!collapsed}
            >
              {collapsed ? (
                <PanelLeftOpen className="size-4" aria-hidden="true" />
              ) : (
                <PanelLeftClose className="size-4" aria-hidden="true" />
              )}
            </button>
            <div data-slot="app-shell-breadcrumbs" className="min-w-0 truncate text-xs">
              {breadcrumbs}
            </div>
          </div>

          <div className="flex shrink-0 items-center gap-2">
            {searchTrigger ?? <SearchTrigger onClick={onSearchClick} />}
            {topBarActions}
          </div>
        </header>

        <main
          data-slot="app-shell-content"
          className={cn(
            'min-w-0 flex-1 px-4 py-5 sm:px-6 lg:px-8 lg:py-7',
            bottomAction && 'pb-24 sm:pb-6',
          )}
        >
          {children}
        </main>

        {bottomAction && (
          <div
            data-slot="app-shell-bottom-action"
            className="fixed inset-x-0 bottom-0 z-30 border-t border-border bg-surface p-3 shadow-card sm:hidden"
          >
            {bottomAction}
          </div>
        )}
      </div>
    </div>
  );
}
