// Profiles (2026-11-17): the crop the upload uses, which names count as
// unset, renaming yourself, and the first-sign-in prompt.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { squareCrop } from './pictureFile';
import { ProfileSection } from './ProfileSection';
import { WelcomeDialog, looksUnset } from './WelcomeDialog';

function me(name: string) {
  return {
    user: { id: 'usr_1', name },
    org: { id: 'org_1', title: 'Acme', role: 'member' },
    teams: [],
    credential: {
      kind: 'personal',
      actor_kind: 'human',
      scopes: ['runbooks:read'],
      projects: null,
      expires_at_ms: Date.now() + 3_600_000,
    },
  };
}

function renderWith(ui: React.ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
  tokenStore.set('trb_pat_test');
  localStorage.clear();
});
afterEach(() => tokenStore.clear());

describe('pictures', () => {
  it('crops a centred square and scales it to at most 512', () => {
    expect(squareCrop(1200, 800)).toEqual({ sx: 200, sy: 0, side: 800, out: 512 });
    expect(squareCrop(300, 400)).toEqual({ sx: 0, sy: 50, side: 300, out: 300 });
  });
});

describe('names', () => {
  it('counts bootstrap defaults and email parts as unset, real names as set', () => {
    for (const name of ['Owner', 'Administrator', 'jonathan', 'j.smith', 'ops+bot']) {
      expect(looksUnset(name)).toBe(true);
    }
    for (const name of ['Jordan Lee', 'Priya', 'Owen Chen']) {
      expect(looksUnset(name)).toBe(false);
    }
  });

  it('renames you from the profile', async () => {
    let sent: unknown = null;
    let name = 'Jordan';
    server.use(
      http.get('*/v1/me', () => HttpResponse.json(me(name))),
      http.patch('*/v1/me', async ({ request }) => {
        sent = await request.json();
        name = (sent as { name: string }).name;
        return HttpResponse.json({ id: 'usr_1', name });
      }),
    );
    renderWith(<ProfileSection />);
    const field = await screen.findByLabelText('Name');
    const user = userEvent.setup();
    await user.clear(field);
    await user.type(field, 'Jordan Lee');
    await user.click(screen.getByRole('button', { name: 'Save name' }));
    await waitFor(() => expect(sent).toEqual({ name: 'Jordan Lee' }));
    expect(await screen.findByText('Saved.')).toBeInTheDocument();
  });
});

describe('the welcome prompt', () => {
  it('asks a person with a placeholder name, once', async () => {
    let sent: unknown = null;
    server.use(
      http.get('*/v1/me', () => HttpResponse.json(me('Owner'))),
      http.patch('*/v1/me', async ({ request }) => {
        sent = await request.json();
        return HttpResponse.json({ id: 'usr_1', name: 'Sam Rivera' });
      }),
    );
    const { unmount } = renderWith(<WelcomeDialog />);
    expect(
      await screen.findByRole('dialog', { name: 'Welcome to The Runbook' }),
    ).toBeInTheDocument();
    const user = userEvent.setup();
    await user.type(screen.getByLabelText('Your name'), 'Sam Rivera');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(sent).toEqual({ name: 'Sam Rivera' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    unmount();
    renderWith(<WelcomeDialog />);
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('leaves a person with a real name alone', async () => {
    server.use(http.get('*/v1/me', () => HttpResponse.json(me('Priya Patel'))));
    renderWith(<WelcomeDialog />);
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
