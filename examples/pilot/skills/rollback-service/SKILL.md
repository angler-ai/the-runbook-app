---
name: rollback-service
description: Roll the service back to its previous release and record the operation ID.
---
# Roll back a service

Pilot fixture for the flagship `post-deploy` runbook (`./skills/rollback-service`).

This step is destructive. Before acting, read the step's earlier logs: if a
rollback operation was already started for this execution, check its status
instead of starting another one (the pilot cannot reconcile external
operation IDs for you).

1. Start the rollback of the service to its previous release with your
   deploy tool, using the execution ID as the operation's idempotency key.
2. Log the deploy tool's operation ID.
3. Report `succeeded` when the previous release is serving, else `failed`
   with a summary.
