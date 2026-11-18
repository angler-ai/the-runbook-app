---
name: check-error-rates
description: Check a service's error rate for one region over a window and attach the dashboard link.
---
# Check error rates

Pilot fixture for the flagship `post-deploy` runbook (`./skills/check-error-rates`).

1. Read `region` and `window` from the step's `with` values (the global
   re-check has neither: use all regions and the last 10 minutes).
2. Query the error rate (5xx responses / all responses) for the service under
   verification over that window.
3. Report `rate` as a number between 0 and 1, the verdict (`pass` when the
   rate is at most 0.01, else `fail`), and the dashboard URL as `link`
   evidence.
