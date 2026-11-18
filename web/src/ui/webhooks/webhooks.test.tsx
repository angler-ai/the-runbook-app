import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expectNoAxeViolations } from '../../test/axe';
import { DeliveryTable, type DeliveryView } from './DeliveryTable';
import { SubscriptionCard, type WebhookView } from './SubscriptionCard';

const mockNow = 1_700_000_000_000;

const mockDeliveries: DeliveryView[] = [
  {
    id: 'del_01',
    webhook_id: 'wh_test',
    event_id: 'evt_01',
    event_type: 'execution.completed',
    state: 'delivered',
    created_at_ms: mockNow - 60_000,
    attempts: [
      {
        number: 1,
        at_ms: mockNow - 59_000,
        status: 200,
        latency_ms: 150,
        outcome: 'delivered',
      },
    ],
  },
  {
    id: 'del_02',
    webhook_id: 'wh_test',
    event_id: 'evt_02',
    event_type: 'step.failed',
    state: 'pending',
    created_at_ms: mockNow - 30_000,
    next_attempt_at_ms: mockNow + 120_000,
    attempts: [
      {
        number: 1,
        at_ms: mockNow - 29_000,
        status: 500,
        latency_ms: 800,
        outcome: 'retry',
      },
    ],
  },
  {
    id: 'del_03',
    webhook_id: 'wh_test',
    event_id: 'evt_03',
    event_type: 'approval.escalated',
    state: 'failed',
    failure: 'secret_unavailable',
    created_at_ms: mockNow - 100_000,
    attempts: [],
  },
];

const mockSubscription: WebhookView = {
  id: 'wh_pagerduty',
  name: 'PagerDuty Incidents',
  url: 'https://events.pagerduty.com/v2/enqueue',
  enabled: true,
  events: ['execution.failed', 'step.failed'],
  admin_notice: null,
  secret_rotated_at_ms: mockNow - 86_400_000 * 2,
  previous_secret_expires_at_ms: mockNow + 86_400_000,
  project_id: 'prj_01',
  runbooks: [],
  tags: ['alerts'],
  created_at_ms: mockNow - 86_400_000 * 10,
  updated_at_ms: mockNow - 86_400_000 * 2,
};

describe('DeliveryTable', () => {
  it('renders event type, status, attempt count, response, retry and duration', async () => {
    const user = userEvent.setup();
    const onLoadMore = vi.fn();

    const { container } = render(
      <DeliveryTable
        deliveries={mockDeliveries}
        now={mockNow}
        hasMore
        onLoadMore={onLoadMore}
      />,
    );

    // Event types
    expect(screen.getByText('execution.completed')).toBeInTheDocument();
    expect(screen.getByText('step.failed')).toBeInTheDocument();
    expect(screen.getByText('approval.escalated')).toBeInTheDocument();

    // Statuses
    expect(screen.getByText('delivered')).toBeInTheDocument();
    expect(screen.getByText('retrying')).toBeInTheDocument();
    expect(screen.getByText('secret_unavailable')).toBeInTheDocument();

    // Attempts and response codes
    expect(screen.getByText('200')).toBeInTheDocument();
    expect(screen.getByText('500')).toBeInTheDocument();

    // Next retry
    expect(screen.getByText('in 2m')).toBeInTheDocument();

    // Duration
    expect(screen.getAllByText('<1s').length).toBeGreaterThan(0);

    // Load more button
    const loadMoreBtn = screen.getByRole('button', { name: 'Load more deliveries' });
    await user.click(loadMoreBtn);
    expect(onLoadMore).toHaveBeenCalled();

    await expectNoAxeViolations(container);
  });

  it('renders empty state when deliveries array is empty', async () => {
    const { container } = render(<DeliveryTable deliveries={[]} />);
    expect(screen.getByText('No webhook deliveries')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('SubscriptionCard', () => {
  it('renders subscription details, health, event filters and secret rotation', async () => {
    const { container } = render(
      <SubscriptionCard subscription={mockSubscription} now={mockNow} />,
    );

    expect(screen.getByText('PagerDuty Incidents')).toBeInTheDocument();
    expect(screen.getByText('Active')).toBeInTheDocument();
    expect(screen.getByText('https://events.pagerduty.com/v2/enqueue')).toBeInTheDocument();
    expect(screen.getByText('execution.failed')).toBeInTheDocument();
    expect(screen.getByText('step.failed')).toBeInTheDocument();
    expect(screen.getByText(/Dual signing active/)).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('renders disabled state with reason notice', async () => {
    const disabledSub: WebhookView = {
      ...mockSubscription,
      enabled: false,
      disabled_reason: 'delivery_failing',
      disabled_at_ms: mockNow - 3600_000,
    };

    const { container } = render(
      <SubscriptionCard subscription={disabledSub} now={mockNow} />,
    );

    expect(screen.getByText('Disabled')).toBeInTheDocument();
    expect(screen.getByText(/delivery_failing/)).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('renders a javascript: subscription URL strictly as plain text (never a link)', async () => {
    const dangerousUrl = 'javascript:alert(1)';
    const dangerousSub: WebhookView = {
      ...mockSubscription,
      url: dangerousUrl,
    };

    const { container } = render(
      <SubscriptionCard subscription={dangerousSub} now={mockNow} />,
    );

    // The text content should literally match the URL
    const urlSpan = container.querySelector('[data-slot="subscription-url"]');
    expect(urlSpan).toBeInTheDocument();
    expect(urlSpan?.textContent).toBe(dangerousUrl);

    // CRITICAL SECURITY ASSERTIONS:
    // 1. No <a> element exists in the container
    expect(container.querySelector('a')).toBeNull();
    // 2. No element in the container has an href attribute
    expect(container.querySelector('[href]')).toBeNull();

    await expectNoAxeViolations(container);
  });
});
