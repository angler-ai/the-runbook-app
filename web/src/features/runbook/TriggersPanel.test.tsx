import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { TriggersPanel } from './TriggersPanel';
import type { RunbookTriggersView } from './triggers';

const TRIGGERS: RunbookTriggersView = {
  schedule: [
    {
      id: 'nightly',
      cron: '0 2 * * *',
      timezone: 'America/Los_Angeles',
      catchUp: '1h',
      inputNames: ['service'],
    },
  ],
  webhook: [
    {
      id: 'on_deploy',
      hasPayloadSchema: true,
      conditional: true,
      deduped: true,
      inputNames: ['service'],
    },
  ],
};

describe('TriggersPanel', () => {
  it('shows schedules and the webhook POST URL, and copies it', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <TriggersPanel
        triggers={TRIGGERS}
        projectRef="prod"
        runbookSlug="deploy-check"
        publishedVersion={3}
        origin="https://runs.example.test/"
      />,
    );

    expect(screen.getByRole('heading', { name: 'Triggers' })).toBeInTheDocument();
    expect(screen.getByText('nightly')).toBeInTheDocument();
    expect(screen.getByText('0 2 * * *')).toBeInTheDocument();
    expect(screen.getByText(/America\/Los_Angeles/)).toBeInTheDocument();
    expect(screen.getByText(/catch up within 1h/i)).toBeInTheDocument();
    expect(screen.getByText(/published v3/i)).toBeInTheDocument();

    const url =
      'https://runs.example.test/v1/projects/prod/runbooks/deploy-check/triggers/on_deploy';
    expect(screen.getByText(`POST ${url}`)).toBeInTheDocument();
    expect(screen.getByText(/authorization: bearer/i)).toBeInTheDocument();
    expect(screen.getByText(/\?token=/)).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Copy on_deploy webhook URL' }));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Copy on_deploy webhook URL' })).toHaveTextContent(
        /copied/i,
      ),
    );
    await expect(navigator.clipboard.readText()).resolves.toBe(url);
    await expectNoAxeViolations(container);
  });

  it('says draft triggers are inactive until published', () => {
    render(
      <TriggersPanel
        triggers={TRIGGERS}
        projectRef="prod"
        runbookSlug="deploy-check"
        publishedVersion={null}
        origin="https://runs.example.test"
      />,
    );
    expect(screen.getByText(/in the draft/i)).toBeInTheDocument();
    expect(screen.getByText(/publish this runbook to activate/i)).toBeInTheDocument();
  });

  it('renders nothing when a runbook has no triggers', () => {
    const { container } = render(
      <TriggersPanel
        triggers={{ schedule: [], webhook: [] }}
        projectRef="prod"
        runbookSlug="deploy-check"
        publishedVersion={3}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it('starts as a one-line summary that opens to the details', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <TriggersPanel
        triggers={TRIGGERS}
        projectRef="prod"
        runbookSlug="deploy-check"
        publishedVersion={3}
        origin="https://runs.example.test/"
      />,
    );
    const details = container.querySelector('details');
    expect(details).not.toBeNull();
    expect(details?.open).toBe(false);
    const summary = container.querySelector('summary') as HTMLElement;
    expect(summary).toHaveTextContent('1 schedule · 1 webhook · nightly, on_deploy');
    expect(summary).toHaveTextContent('Active from published v3');
    await user.click(summary);
    expect(details?.open).toBe(true);
  });
});
