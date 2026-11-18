-- WebhookDO 0002 (gate fix D3): the object's lifecycle.
-- `retired`: the subscription was deleted and its log purged; the object
-- refuses every later write (a fan-out that raced the deletion), so it can
-- never come back with an owner and an alarm.
CREATE TABLE retired (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  at_ms INTEGER NOT NULL
);
-- `alarm_backoff`: consecutive failed alarm passes (D1 down, no sealer key)
-- and the earliest time the next may run; a pass that works clears it.
CREATE TABLE alarm_backoff (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  failures INTEGER NOT NULL,
  until_ms INTEGER NOT NULL
);
