import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { tokenStore } from '../../api/token-store';
import { afterEach, describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { OrgProvider } from '../context/OrgContext';
import { WebhooksPage } from './WebhooksPage';
import type { DeliveryView, WebhookView } from './hooks';

afterEach(() => tokenStore.clear());

const PRJ = 'prj_1';

const sampleWebhook1: WebhookView = {
  id: 'whk_111111111111',
  project_id: PRJ,
  name: 'Slack Alerts',
  url: 'https://hooks.slack.com/services/T00/B00/X00',
  events: ['*'],
  runbooks: [],
  tags: ['prod'],
  enabled: true,
  secret_rotated_at_ms: Date.now() - 3600000,
  previous_secret_expires_at_ms: null,
  failing_since_ms: null,
  disabled_at_ms: null,
  disabled_reason: null,
  admin_notice: null,
  created_at_ms: Date.now() - 86400000,
  updated_at_ms: Date.now() - 3600000,
};

const sampleWebhook2: WebhookView = {
  id: 'whk_222222222222',
  project_id: PRJ,
  name: 'PagerDuty',
  url: 'https://events.pagerduty.com/v2/enqueue',
  events: ['execution.failed'],
  runbooks: [],
  tags: [],
  enabled: false,
  secret_rotated_at_ms: Date.now() - 7200000,
  previous_secret_expires_at_ms: null,
  failing_since_ms: null,
  disabled_at_ms: Date.now() - 3600000,
  disabled_reason: 'Automatic disable after delivery failures',
  admin_notice: null,
  created_at_ms: Date.now() - 86400000,
  updated_at_ms: Date.now() - 3600000,
};

const sampleDelivery: DeliveryView = {
  id: 'del_111111111111',
  webhook_id: sampleWebhook1.id,
  event_type: 'execution.started',
  event_id: 'evt_1',
  created_at_ms: Date.now() - 60000,
  state: 'delivered',
  next_attempt_at_ms: null,
  failure: null,
  attempts: [
    {
      number: 1,
      at_ms: Date.now() - 60000,
      status: 200,
      latency_ms: 120,
      outcome: 'delivered',
      response_snippet: '{"ok":true}',
    },
  ],
};

function renderWebhooksPage() {
  tokenStore.set('trb_pat_test');
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OrgProvider>
        <WebhooksPage projectId={PRJ} />
      </OrgProvider>
    </QueryClientProvider>,
  );
}

function baseHandlers(isAdmin = true) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: isAdmin ? 'admin' : 'member' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes: isAdmin
            ? ['runbooks:read', 'webhooks:manage', 'project:admin']
            : ['runbooks:read'],
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
        items: [{ id: PRJ, org_id: 'org_1', slug: 'prod', title: 'Production' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/v1/projects/:p/members', () =>
      HttpResponse.json({
        items: [{ user_id: 'usr_1', role: isAdmin ? 'admin' : 'viewer' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/webhooks', () =>
      HttpResponse.json({
        items: [sampleWebhook1, sampleWebhook2],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/webhooks/:id/deliveries', () =>
      HttpResponse.json({
        items: [sampleDelivery],
        next_cursor: null,
      }),
    ),
  ];
}

describe('WebhooksPage', () => {
  it('renders webhooks list and deliveries for an admin, passing axe check', async () => {
    server.use(...baseHandlers(true));

    const { container } = renderWebhooksPage();

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Webhooks' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /^Slack Alerts/ })).toHaveAttribute(
        'aria-current',
        'true',
      );
      expect(screen.getByRole('button', { name: /^PagerDuty/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Add webhook/i })).toBeInTheDocument();
    });

    // Check delivery log loaded for active webhook
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Slack Alerts' })).toBeInTheDocument();
      expect(screen.getByRole('heading', { name: 'Deliveries' })).toBeInTheDocument();
      expect(screen.getByText('execution.started')).toBeInTheDocument();
    });

    await expectNoAxeViolations(container);
  });

  it('selects a webhook by clicking anywhere on its row, the first one by default', async () => {
    server.use(...baseHandlers(true));
    renderWebhooksPage();
    expect(await screen.findByRole('heading', { name: 'Slack Alerts' })).toBeInTheDocument();
    // Details live in the main panel, not the list.
    expect(screen.getAllByText(sampleWebhook1.url)).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: /^PagerDuty/ }));
    expect(await screen.findByRole('heading', { name: 'PagerDuty' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^PagerDuty/ })).toHaveAttribute(
      'aria-current',
      'true',
    );
    expect(screen.getByText(sampleWebhook2.url)).toBeInTheDocument();
    expect(screen.queryByText(sampleWebhook1.url)).not.toBeInTheDocument();
  });

  it('shows clear refusal message when webhooks:manage scope is missing, passing axe check', async () => {
    server.use(...baseHandlers(false));

    const { container } = renderWebhooksPage();

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Webhooks' })).toBeInTheDocument();
      expect(
        screen.getByText(/Sign in with email or Google to manage webhooks/i),
      ).toBeInTheDocument();
    });

    expect(screen.queryByRole('button', { name: /Add webhook/i })).not.toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('renders empty state when there are no webhooks, passing axe check', async () => {
    server.use(
      ...baseHandlers(true).filter((h) => !String(h.info.path).includes('/webhooks')),
      http.get('*/v1/projects/:p/webhooks', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
    );

    const { container } = renderWebhooksPage();

    await waitFor(() => {
      expect(screen.getByText('No webhooks configured')).toBeInTheDocument();
    });

    await expectNoAxeViolations(container);
  });

  it('sends a test ping to a webhook', async () => {
    let testCalled = false;
    server.use(
      ...baseHandlers(true),
      http.post('*/v1/projects/:p/webhooks/:id/test', () => {
        testCalled = true;
        return HttpResponse.json(sampleDelivery, { status: 202 });
      }),
    );

    renderWebhooksPage();

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Slack Alerts' })).toBeInTheDocument();
    });

    const testBtn = screen.getByRole('button', { name: /Send test event to Slack Alerts/i });
    await userEvent.click(testBtn);

    await waitFor(() => {
      expect(testCalled).toBe(true);
      expect(screen.getByText(/Test event sent to "Slack Alerts"/i)).toBeInTheDocument();
    });
  });

  it('rotates webhook secret and reveals new secret once', async () => {
    let rotatedCalled = false;
    server.use(
      ...baseHandlers(true),
      http.post('*/v1/projects/:p/webhooks/:id/rotate-secret', () => {
        rotatedCalled = true;
        return HttpResponse.json({
          webhook: sampleWebhook1,
          secret: 'whsec_new_super_secret_key_12345',
        });
      }),
    );

    renderWebhooksPage();

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Slack Alerts' })).toBeInTheDocument();
    });

    const rotateBtn = screen.getByRole('button', { name: /Rotate secret for Slack Alerts/i });
    await userEvent.click(rotateBtn);

    // Confirm dialog opened
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Rotate signing secret' })).toBeInTheDocument();
    });

    const confirmRotateBtn = screen.getByRole('button', { name: 'Rotate secret' });
    await userEvent.click(confirmRotateBtn);

    await waitFor(() => {
      expect(rotatedCalled).toBe(true);
      expect(screen.getByDisplayValue('whsec_new_super_secret_key_12345')).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Copy new signing secret/i })).toBeInTheDocument();
    });
  });

  it('inspects delivery and triggers redeliver', async () => {
    let redeliverCalled = false;
    server.use(
      ...baseHandlers(true),
      http.post('*/v1/projects/:p/webhooks/:id/deliveries/:d/redeliver', () => {
        redeliverCalled = true;
        return HttpResponse.json(sampleDelivery, { status: 202 });
      }),
    );

    renderWebhooksPage();

    await waitFor(() => {
      expect(screen.getByText('execution.started')).toBeInTheDocument();
    });

    // Click the delivery text/cell
    fireEvent.click(screen.getByText('execution.started'));

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Delivery details' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Redeliver/i })).toBeInTheDocument();
    });

    const redeliverBtn = screen.getByRole('button', { name: /Redeliver/i });
    await userEvent.click(redeliverBtn);

    await waitFor(() => {
      expect(redeliverCalled).toBe(true);
      expect(
        screen.getByText(/Redelivery queued for event execution.started/i),
      ).toBeInTheDocument();
    });
  });

  it('handles error state with retry', async () => {
    let callCount = 0;
    server.use(
      ...baseHandlers(true).filter((h) => !String(h.info.path).includes('/webhooks')),
      http.get('*/v1/projects/:p/webhooks', () => {
        callCount++;
        if (callCount === 1) {
          return new HttpResponse(null, { status: 500 });
        }
        return HttpResponse.json({ items: [sampleWebhook1], next_cursor: null });
      }),
      http.get('*/v1/projects/:p/webhooks/:id/deliveries', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
    );

    renderWebhooksPage();

    await waitFor(() => {
      expect(screen.getByText('Could not load webhooks')).toBeInTheDocument();
    });

    const retryBtn = screen.getByRole('button', { name: /retry/i });
    await userEvent.click(retryBtn);

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Slack Alerts' })).toBeInTheDocument();
    });
  });
});

describe('WebhooksPage create panel', () => {
  it('creates from the right panel with picked events and shows the secret once', async () => {
    let created = null as { events?: string[] } | null;
    server.use(
      ...baseHandlers(true),
      http.get('*/v1/projects/:p/runbooks', () =>
        HttpResponse.json({
          items: [
            {
              runbook_id: 'rb_1',
              slug: 'post-deploy',
              origin: 'ui',
              archived: false,
              title: 'Post-deploy',
              latest_version: 1,
            },
          ],
          next_cursor: null,
        }),
      ),
      http.post('*/v1/projects/:p/webhooks', async ({ request }) => {
        created = (await request.json()) as { events?: string[] };
        return HttpResponse.json(
          { ...sampleWebhook1, id: 'whk_new', secret: 'whsec_once' },
          { status: 201 },
        );
      }),
    );
    renderWebhooksPage();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /Add webhook/i }));
    // Nothing is selected by default, and creation requires a pick.
    expect(screen.getByText(/0 of \d+ events selected/)).toBeInTheDocument();
    await user.type(screen.getByLabelText(/^Name$/i), 'Ops channel');
    await user.type(screen.getByLabelText(/Endpoint URL/i), 'https://example.com/hook');
    await user.click(screen.getByRole('button', { name: 'Create webhook' }));
    expect(await screen.findByText(/Pick at least one event/)).toBeInTheDocument();
    await user.click(screen.getByRole('checkbox', { name: /Run started/ }));
    await user.click(screen.getByRole('button', { name: 'Create webhook' }));
    expect(await screen.findByDisplayValue('whsec_once')).toBeInTheDocument();
    expect(created?.events).toEqual(['execution.started']);
  });
});
