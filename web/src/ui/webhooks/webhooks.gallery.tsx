import { defineGallery } from '../../gallery/registry';
import { DeliveryTable, type DeliveryView } from './DeliveryTable';
import { SubscriptionCard, type WebhookView } from './SubscriptionCard';

const mockDeliveries: DeliveryView[] = [
  {
    id: 'del_01HXYZ11111111111111111111',
    webhook_id: 'wh_prod_alerts',
    event_id: 'evt_01HXYZ00001',
    event_type: 'execution.completed',
    state: 'delivered',
    created_at_ms: Date.now() - 120_000,
    attempts: [
      {
        number: 1,
        at_ms: Date.now() - 119_500,
        status: 200,
        latency_ms: 245,
        outcome: 'delivered',
        response_snippet: '{"ok": true}',
      },
    ],
  },
  {
    id: 'del_01HXYZ22222222222222222222',
    webhook_id: 'wh_prod_alerts',
    event_id: 'evt_01HXYZ00002',
    event_type: 'step.failed',
    state: 'pending',
    created_at_ms: Date.now() - 45_000,
    next_attempt_at_ms: Date.now() + 60_000,
    attempts: [
      {
        number: 1,
        at_ms: Date.now() - 44_000,
        status: 504,
        latency_ms: 1200,
        outcome: 'retry',
        error: 'Gateway Timeout',
      },
    ],
  },
  {
    id: 'del_01HXYZ33333333333333333333',
    webhook_id: 'wh_prod_alerts',
    event_id: 'evt_01HXYZ00003',
    event_type: 'approval.escalated',
    state: 'failed',
    failure: 'secret_unavailable',
    created_at_ms: Date.now() - 3_600_000,
    attempts: [
      {
        number: 1,
        at_ms: Date.now() - 3_599_000,
        latency_ms: 12,
        outcome: 'failed',
        error: 'secret_unavailable',
      },
    ],
  },
];

const mockActiveSubscription: WebhookView = {
  id: 'wh_prod_alerts',
  name: 'PagerDuty Production Incidents',
  url: 'https://events.pagerduty.com/v2/enqueue',
  enabled: true,
  events: ['execution.failed', 'step.failed', 'approval.escalated'],
  admin_notice: null,
  secret_rotated_at_ms: Date.now() - 86_400_000 * 5,
  previous_secret_expires_at_ms: Date.now() + 86_400_000 * 2,
  project_id: 'prj_core',
  runbooks: [],
  tags: ['production'],
  created_at_ms: Date.now() - 86_400_000 * 30,
  updated_at_ms: Date.now() - 86_400_000 * 5,
};

const mockDisabledSubscription: WebhookView = {
  id: 'wh_slack_channel',
  name: 'Slack Deployment Notifications',
  url: 'https://hooks.slack.com/services/T00/B00/XXXXX',
  enabled: false,
  disabled_reason: 'delivery_failing',
  disabled_at_ms: Date.now() - 86_400_000,
  admin_notice: {
    at_ms: Date.now() - 86_400_000,
    failing_since_ms: Date.now() - 86_400_000 * 3,
    kind: 'webhook.disabled',
    message: 'Endpoint returned 404 Not Found repeatedly.',
    last_failure: 'http_404',
  },
  events: ['execution.started', 'execution.completed'],
  secret_rotated_at_ms: Date.now() - 86_400_000 * 60,
  previous_secret_expires_at_ms: null,
  project_id: 'prj_core',
  runbooks: [],
  tags: ['slack'],
  created_at_ms: Date.now() - 86_400_000 * 60,
  updated_at_ms: Date.now() - 86_400_000,
};

export default [
  defineGallery({
    id: 'webhook-logs',
    group: 'Webhooks',
    title: 'Delivery Table & Subscription Card',
    examples: [
      {
        name: 'Active Subscription Card',
        render: () => (
          <div className="max-w-2xl">
            <SubscriptionCard subscription={mockActiveSubscription} />
          </div>
        ),
      },
      {
        name: 'Disabled Subscription Card',
        render: () => (
          <div className="max-w-2xl">
            <SubscriptionCard subscription={mockDisabledSubscription} />
          </div>
        ),
      },
      {
        name: 'Webhook Deliveries Table',
        render: () => (
          <div className="max-w-4xl">
            <DeliveryTable
              deliveries={mockDeliveries}
              hasMore
              onLoadMore={() => alert('Load more clicked')}
            />
          </div>
        ),
      },
      {
        name: 'Empty Delivery Table',
        render: () => <DeliveryTable deliveries={[]} />,
      },
    ],
  }),
];
