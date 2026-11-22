// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, test, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { OfflineBanner, SetupCard, VaultIdentityFingerprint, projectAllowed } from './presentation';
import { Checklist, Capacity } from './Overview';

afterEach(cleanup);
const group = { id: 'rg_1', name: 'Primary', projects: ['p_1'] };
const runner = {
  id: 'rn_1',
  group_id: group.id,
  name: 'Worker',
  status: 'ready',
  running: 1,
  capacity: 3,
  version: '1',
};

test('no_runner_shows_setup_card_to_admins_only', () => {
  const view = render(<SetupCard admin={false} />);
  expect(screen.queryByText('Set up a runner')).toBeNull();
  view.rerender(<SetupCard admin />);
  expect(screen.getByText('Set up a runner')).toBeVisible();
});

test('agents_nav_only_when_project_allowed', () => {
  const status = {
    status: 'offline' as const,
    install_id: 'ri_1',
    runners: [runner],
    groups: [group],
  };
  expect(projectAllowed(status, 'p_1')).toBe(true);
  expect(projectAllowed(status, 'p_2')).toBe(false);
  expect(projectAllowed(undefined, 'p_1')).toBe(false);
});

test('offline_banner_disables_edits', () => {
  render(
    <OfflineBanner offline>
      <button>Edit</button>
    </OfflineBanner>,
  );
  expect(screen.getByRole('status')).toHaveTextContent('Runner is offline');
  expect(screen.getByRole('button', { name: 'Edit' })).toBeDisabled();
});

test('checklist_items_link_to_fixes', () => {
  render(<Checklist online hasProject={false} />);
  expect(screen.getByRole('link', { name: 'A model profile works' })).toHaveAttribute(
    'href',
    '?tab=providers',
  );
  expect(screen.getByRole('link', { name: 'A project is allowed' })).toHaveAttribute(
    'href',
    '#runner-projects',
  );
});

test('group_and_runner_columns_appear_when_more_than_one', () => {
  const view = render(<Capacity groups={[group]} runners={[runner]} />);
  expect(screen.queryByRole('columnheader', { name: 'Group' })).toBeNull();
  expect(screen.queryByRole('columnheader', { name: 'Runner' })).toBeNull();
  view.rerender(
    <Capacity
      groups={[group, { ...group, id: 'rg_2' }]}
      runners={[runner, { ...runner, id: 'rn_2' }]}
    />,
  );
  expect(screen.getByRole('columnheader', { name: 'Group' })).toBeVisible();
  expect(screen.getByRole('columnheader', { name: 'Runner' })).toBeVisible();
});

test('checklist requires a verified model profile', () => {
  const { rerender } = render(<Checklist online profiles={[{ verified_at: null }]} hasProject />);
  const item = () => screen.getByRole('link', { name: 'A model profile works' }).closest('li');
  expect(item()).toHaveTextContent('To do');
  rerender(<Checklist online profiles={[{ verified_at: '2026-11-20T00:00:00Z' }]} hasProject />);
  expect(item()).toHaveTextContent('Done');
});

test('shows the SHA-256 fingerprint of the enrolled vault identity', async () => {
  const expected = '66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925';
  const digest = vi
    .fn()
    .mockResolvedValue(
      Uint8Array.from(expected.match(/../g)!, (pair) => parseInt(pair, 16)).buffer,
    );
  vi.stubGlobal('crypto', { subtle: { digest } });
  try {
    render(
      <QueryClientProvider client={new QueryClient()}>
        <VaultIdentityFingerprint publicKey={'A'.repeat(43)} />
      </QueryClientProvider>,
    );
    expect(await screen.findByText(expected)).toBeVisible();
    expect(digest).toHaveBeenCalledWith('SHA-256', new Uint8Array(32));
  } finally {
    vi.unstubAllGlobals();
  }
});
