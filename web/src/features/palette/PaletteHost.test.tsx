import { createMemoryHistory } from '@tanstack/react-router';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { useState } from 'react';
import { App } from '../../App';
import { tokenStore } from '../../api/token-store';
import { createAppRouter } from '../../routes/router';
import { server } from '../../test/msw/server';
import { ConfirmDialog } from '../../ui/actions';
import { isInputElement, rememberRecent, useCommandPaletteHotkey } from './PaletteHost';

function Harness() {
  const [open, setOpen] = useState(false);
  useCommandPaletteHotkey(open, setOpen);
  return (
    <div>
      <input aria-label="Type here" />
      <textarea aria-label="Write here" />
      <p>{open ? 'palette open' : 'palette closed'}</p>
    </div>
  );
}

describe('command palette hotkey', () => {
  it('does not fire while typing in an input or textarea', () => {
    render(<Harness />);
    const input = screen.getByLabelText('Type here');
    input.focus();
    fireEvent.keyDown(input, { key: 'k', ctrlKey: true });
    expect(screen.getByText('palette closed')).toBeInTheDocument();
    const area = screen.getByLabelText('Write here');
    area.focus();
    fireEvent.keyDown(area, { key: 'K', metaKey: true });
    expect(screen.getByText('palette closed')).toBeInTheDocument();
  });

  it('toggles from the document body and reports input elements', () => {
    render(<Harness />);
    fireEvent.keyDown(document.body, { key: 'k', ctrlKey: true });
    expect(screen.getByText('palette open')).toBeInTheDocument();
    fireEvent.keyDown(document.body, { key: 'k', metaKey: true });
    expect(screen.getByText('palette closed')).toBeInTheDocument();
    expect(isInputElement(screen.getByLabelText('Type here'))).toBe(true);
    expect(isInputElement(document.body)).toBe(false);
  });
});

// U20: the palette as the shell hosts it.
describe('the palette in the app shell (U20)', () => {
  const empty = () => HttpResponse.json({ items: [], next_cursor: null });
  function shellHandlers() {
    server.use(
      http.get('*/v1/me', () =>
        HttpResponse.json({
          user: { id: 'usr_1', name: 'Alice' },
          org: { id: 'org_1', title: 'Acme', role: 'operator' },
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
      http.get('*/v1/orgs/:org/users', empty),
      http.get('*/v1/orgs/:org/teams', empty),
      http.get('*/v1/projects/:p/runbooks', empty),
      http.get('*/v1/projects/:p/executions', empty),
      http.get('*/v1/me/tasks', empty),
    );
  }

  async function renderShell() {
    shellHandlers();
    tokenStore.set('trb_pat_good');
    const router = createAppRouter(createMemoryHistory({ initialEntries: ['/tasks'] }));
    render(<App router={router} />);
    expect(await screen.findByRole('heading', { name: 'My tasks' })).toBeInTheDocument();
  }

  const palette = () => screen.queryByRole('dialog', { name: 'Command Palette' });
  const pressCmdK = () => fireEvent.keyDown(document.body, { key: 'k', metaKey: true });

  afterEach(() => {
    tokenStore.clear();
    sessionStorage.clear();
  });

  it('shows the recents as they are when it opens, not as they were at first render', async () => {
    await renderShell();
    pressCmdK();
    expect(await screen.findByRole('dialog', { name: 'Command Palette' })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: /Verify api v2/ })).not.toBeInTheDocument();
    pressCmdK();
    await waitFor(() => expect(palette()).not.toBeInTheDocument());

    // Let the shell settle, so nothing else re-renders the palette host.
    await new Promise((r) => setTimeout(r, 300));
    // A run opened since (the palette records it as it navigates).
    rememberRecent({ kind: 'execution', id: 'exe_1', label: 'Verify api v2', projectRef: 'prod' });
    pressCmdK();
    const dialog = await screen.findByRole('dialog', { name: 'Command Palette' });
    expect(within(dialog).getByRole('option', { name: /Verify api v2/ })).toBeInTheDocument();
  });

  it('⌘K does not open the palette over another open dialog', async () => {
    await renderShell();
    const confirm = render(
      <ConfirmDialog
        open
        onOpenChange={() => {}}
        title="Cancel this run?"
        description="Cancelling stops the run."
        confirmLabel="Cancel run"
        onConfirm={() => {}}
      />,
    );
    expect(await screen.findByRole('alertdialog')).toBeInTheDocument();
    pressCmdK();
    await new Promise((r) => setTimeout(r, 50));
    expect(palette()).not.toBeInTheDocument();

    // Once the dialog is gone, ⌘K opens the palette again.
    confirm.unmount();
    await waitFor(() => expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument());
    pressCmdK();
    expect(await screen.findByRole('dialog', { name: 'Command Palette' })).toBeInTheDocument();
  });
});
