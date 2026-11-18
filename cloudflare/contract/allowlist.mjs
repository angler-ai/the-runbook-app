// CF9: the known, recorded divergences between the native build and the
// Cloudflare build. A difference the suite finds must match an entry here
// (the call's label and a JSON path under the recorded answer), or the run
// fails. `exercised: true` entries must match at least once, so a fixed
// divergence is noticed and removed; `exercised: false` entries document a
// recorded divergence the sequential scenario cannot reach.
export const ALLOWED = [
  {
    label: 'meta',
    path: 'body.schema',
    exercised: true,
    reason:
      'UPD1: each build reports its own storage schema: native the Postgres migration ' +
      'number (`postgres`), Workers the D1 and Durable Object migration heads (`d1`, ' +
      '`execution_do`, `org_do`, `webhook_do`). The release (`body.version`) is the same ' +
      'workspace version on both and is compared.',
  },
  {
    label: 'meta',
    path: 'body.limits.uploads',
    exercised: true,
    reason:
      'CF7-2 (revisited by CF12): the Cloudflare build advertises the product Free ' +
      "edition's upload limits (runbook_core::uploads::limits::free: 10 MB files, counts, " +
      'a 5 GB quota) with no image types; native self-hosted has none. Not a Workers plan ' +
      'limit: the signed PUT is read whole into an isolate of 128 MB on every plan, and ' +
      'images wait for a Workers pipeline that strips their metadata.',
  },
  {
    label: '*',
    path: 'webhook DNS vetting',
    exercised: false,
    reason:
      'CF4 "Transport": Workers cannot resolve a hostname, so a webhook URL naming a host ' +
      'that resolves to a private address is accepted at creation on Cloudflare (egress ' +
      'cannot reach private networks) and refused natively (webhooks.target_forbidden). ' +
      'Both builds here allow private targets, so the scenario cannot show it.',
  },
  {
    label: '*',
    path: 'concurrent sessions',
    exercised: false,
    reason:
      'CF5: a session over the object protocol is optimistic, so two concurrent writers ' +
      'to one run get a 409 conflict on Cloudflare where the native store makes the second ' +
      'wait. The scenario is sequential.',
  },
  {
    label: '*',
    path: 'Workers request limits',
    exercised: false,
    reason:
      'CF12 (Workers Paid): a portable import, an operator route outside the scenario, ' +
      'is refused past 10 MiB of archive (the isolate holds it whole), past a worst case ' +
      'of 9,900 subrequests (10,000 per invocation less the route reserve) or past 10,000 ' +
      'statements in one D1 batch (D1 holds a batch to 30 s), and while another import ' +
      'runs in the isolate (import.busy); an export is refused past the same subrequest ' +
      'budget (export.too_large). Native imports and exports any size. Native allows skill ' +
      'uploads up to RUNBOOK_MAX_REQUEST_BYTES (64 MiB by default); the api Worker caps ' +
      '/v1 and /mcp bodies at 2 MiB + 64 KiB, and this suite sets native to the same.',
  },
];
