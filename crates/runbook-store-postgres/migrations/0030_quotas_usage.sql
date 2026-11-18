-- CF6: monthly execution quotas (PRD §17, D25). Every start is counted in
-- its organization's UTC month by the store clock; a configured limit
-- refuses the start that would exceed it (limits are off by default when
-- self-hosted). A month's row is created by its first start, so months
-- roll over lazily and old months stay for billing.
CREATE TABLE runbook.usage_monthly (
    org_id text NOT NULL,
    month text NOT NULL,
    executions_started bigint NOT NULL CHECK (executions_started >= 0),
    PRIMARY KEY (org_id, month)
);

-- The month each counted start was counted in, so a run cancelled while
-- queued is refunded from the right month and a retried start never counts
-- twice. A run's deletion takes its row (no refund: it ran).
CREATE TABLE runbook.quota_reservation (
    execution_id text PRIMARY KEY REFERENCES runbook.execution_owner ON DELETE CASCADE,
    org_id text NOT NULL,
    month text NOT NULL,
    created_at_ms bigint NOT NULL
);
CREATE INDEX quota_reservation_month ON runbook.quota_reservation (org_id, month);
