import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { Button } from '../primitives/button';
import { AppShell } from './AppShell';
import { EmptyState } from './EmptyState';
import { ErrorState } from './ErrorState';
import { PageHeader } from './PageHeader';
import { SidebarNav, type SidebarNavItem } from './SidebarNav';
import { Skeleton } from './Skeleton';
import { SplitPane } from './SplitPane';
import { BookOpen, CheckSquare, Inbox, Play } from 'lucide-react';

describe('SidebarNav', () => {
  const items: SidebarNavItem[] = [
    { id: 'runbooks', label: 'Runbooks', href: '/runbooks', icon: BookOpen, active: true },
    { id: 'executions', label: 'Executions', href: '/executions', icon: Play },
    { id: 'tasks', label: 'My tasks', href: '/tasks', icon: CheckSquare, count: 5 },
  ];

  it('renders items with active state and count badge, and is axe-clean', async () => {
    const { container } = render(<SidebarNav items={items} />);
    expect(screen.getByRole('link', { name: /Runbooks/ })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByText('5')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('compact: icons only, each named by its label and count, with a tooltip', async () => {
    const { container } = render(<SidebarNav compact items={items} />);
    expect(screen.getByRole('link', { name: 'Runbooks' })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('link', { name: 'My tasks, 5' })).toHaveAttribute('title', 'My tasks');
    expect(screen.queryByText('Executions', { selector: ':not(.sr-only)' })).toBeNull();
    await expectNoAxeViolations(container);
  });

  it('uses renderLink prop when provided', () => {
    render(
      <SidebarNav
        items={items}
        renderLink={({ item, className, children }) => (
          <span data-testid={`custom-${item.id}`} className={className}>
            {children}
          </span>
        )}
      />,
    );
    expect(screen.getByTestId('custom-runbooks')).toBeInTheDocument();
    expect(screen.getByTestId('custom-tasks')).toHaveTextContent('My tasks');
  });
});

describe('AppShell', () => {
  it('renders sidebar, top bar breadcrumbs, ⌘K trigger, and is axe-clean', async () => {
    const user = userEvent.setup();
    let searchClicked = false;

    const { container } = render(
      <AppShell
        sidebar={<div data-testid="sidebar-content">Sidebar Content</div>}
        breadcrumbs={<span>Runbooks / Post-deploy</span>}
        onSearchClick={() => {
          searchClicked = true;
        }}
        bottomAction={<Button>Quick Action</Button>}
      >
        <div>Main Canvas Content</div>
      </AppShell>,
    );

    expect(screen.getByTestId('sidebar-content')).toBeInTheDocument();
    expect(screen.getByText('Runbooks / Post-deploy')).toBeInTheDocument();
    expect(screen.getByText('Main Canvas Content')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Quick Action' })).toBeInTheDocument();

    const searchBtn = screen.getByRole('button', { name: 'Search ⌘K (open command palette)' });
    await user.click(searchBtn);
    expect(searchClicked).toBe(true);

    await expectNoAxeViolations(container);
  });

  it('satisfies label-in-name for command palette trigger (WCAG 2.5.3) and is axe-clean', async () => {
    const { container } = render(
      <AppShell sidebar={<div>Sidebar</div>}>
        <div>Content</div>
      </AppShell>,
    );
    const trigger = screen.getByRole('button', { name: /^Search/i });
    expect(trigger).toHaveAccessibleName('Search ⌘K (open command palette)');
    await expectNoAxeViolations(container);
  });

  it('collapses the sidebar to the rail, and back', async () => {
    localStorage.removeItem('rb.sidebar-collapsed');
    const user = userEvent.setup();
    render(
      <AppShell sidebar={<p>Full sidebar</p>} rail={<p>Icon rail</p>}>
        <p>Page</p>
      </AppShell>,
    );
    expect(screen.queryByText('Icon rail')).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Hide sidebar' }));
    expect(screen.getByText('Icon rail')).toBeInTheDocument();
    expect(localStorage.getItem('rb.sidebar-collapsed')).toBe('1');
    await user.click(screen.getByRole('button', { name: 'Show sidebar' }));
    expect(screen.queryByText('Icon rail')).toBeNull();
  });

  it('opens mobile drawer when clicking the menu button', async () => {
    const user = userEvent.setup();
    render(
      <AppShell sidebar={<div>Mobile Sidebar Content</div>}>
        <div>Body</div>
      </AppShell>,
    );

    const menuBtn = screen.getByRole('button', { name: 'Open navigation menu' });
    await user.click(menuBtn);

    expect(screen.getByRole('dialog', { name: 'Navigation Menu' })).toBeInTheDocument();
    await expectNoAxeViolations(document.body);
  });

  it('closes the mobile drawer when a link in it is followed (U20)', async () => {
    const user = userEvent.setup();
    const followed: string[] = [];
    render(
      <AppShell
        sidebar={
          <>
            <p>Acme</p>
            <SidebarNav
              ariaLabel="Primary"
              items={[
                { id: 'tasks', label: 'My tasks', href: '/tasks' },
                { id: 'executions', label: 'Executions', href: '/executions' },
              ]}
              // Like the router's Link: client-side, so the page never reloads.
              renderLink={({ item, className, children }) => (
                <a
                  href={item.href}
                  className={className}
                  onClick={(event) => {
                    event.preventDefault();
                    followed.push(item.href);
                  }}
                >
                  {children}
                </a>
              )}
            />
          </>
        }
      >
        <div>Body</div>
      </AppShell>,
    );

    await user.click(screen.getByRole('button', { name: 'Open navigation menu' }));
    const drawer = screen.getByRole('dialog', { name: 'Navigation Menu' });
    // A click on anything but a link leaves it open.
    await user.click(within(drawer).getByText('Acme'));
    expect(screen.getByRole('dialog', { name: 'Navigation Menu' })).toBeInTheDocument();

    await user.click(within(drawer).getByRole('link', { name: 'Executions' }));
    expect(followed).toEqual(['/executions']);
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Navigation Menu' })).not.toBeInTheDocument(),
    );
  });
});

describe('PageHeader', () => {
  it('renders title, description, breadcrumbs, and actions, and is axe-clean', async () => {
    const { container } = render(
      <PageHeader
        breadcrumbs={<span>Home / Projects</span>}
        title="Production Deployment"
        description="Run verification steps before promotion."
        actions={<Button size="sm">Promote</Button>}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Production Deployment' }),
    ).toBeInTheDocument();
    expect(screen.getByText('Run verification steps before promotion.')).toBeInTheDocument();
    expect(screen.getByText('Home / Projects')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Promote' })).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });
});

describe('SplitPane', () => {
  it('renders left and right pane content and is axe-clean', async () => {
    const { container } = render(
      <SplitPane left={<div>Left Pane Content</div>} right={<div>Right Pane Content</div>} />,
    );

    expect(screen.getByText('Left Pane Content')).toBeInTheDocument();
    expect(screen.getByText('Right Pane Content')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('EmptyState', () => {
  it('renders icon, title, description and action, and is axe-clean', async () => {
    const { container } = render(
      <EmptyState
        icon={Inbox}
        title="No items found"
        description="Try adjusting your filters or search terms."
        action={<Button size="sm">Create new</Button>}
      />,
    );

    expect(screen.getByRole('heading', { level: 3, name: 'No items found' })).toBeInTheDocument();
    expect(screen.getByText('Try adjusting your filters or search terms.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Create new' })).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('Skeleton', () => {
  it('renders skeleton element with aria-hidden and is axe-clean', async () => {
    const { container } = render(
      <div>
        <Skeleton className="h-4 w-32" data-testid="test-skeleton" />
      </div>,
    );

    const skeleton = screen.getByTestId('test-skeleton');
    expect(skeleton).toHaveAttribute('aria-hidden', 'true');
    expect(skeleton).toHaveClass('animate-pulse');
    await expectNoAxeViolations(container);
  });
});

describe('ErrorState', () => {
  it('renders code and requestId without raw bodies, and is axe-clean', async () => {
    const user = userEvent.setup();
    let retried = false;

    const { container } = render(
      <ErrorState
        title="Failed to load step"
        message="The requested execution step was not found."
        code="step.not_found"
        requestId="req_01HX4A"
        onRetry={() => {
          retried = true;
        }}
      />,
    );

    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(
      screen.getByRole('heading', { level: 3, name: 'Failed to load step' }),
    ).toBeInTheDocument();
    expect(screen.getByText('The requested execution step was not found.')).toBeInTheDocument();
    expect(screen.getByText('step.not_found')).toBeInTheDocument();
    expect(screen.getByText('req_01HX4A')).toBeInTheDocument();

    const retryBtn = screen.getByRole('button', { name: 'Retry' });
    await user.click(retryBtn);
    expect(retried).toBe(true);

    await expectNoAxeViolations(container);
  });
});
