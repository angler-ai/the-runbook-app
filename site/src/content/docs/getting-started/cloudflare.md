---
title: Quick start on Cloudflare
description: Deploy the Workers edition to your own Cloudflare account.
sidebar:
  order: 2
  badge: Cloudflare
---

The Cloudflare edition runs on Workers, Durable Objects, D1 and R2 in your
own account. It needs the **Workers Paid** plan: email sign-in codes need
it, and it lifts the Free plan's CPU, subrequest and D1 query limits. To try
The Runbook without it, use the [Docker quick start](/getting-started/docker/).

Run every command from `cloudflare/` in a checkout, with that directory's
wrangler (`npx wrangler`).

## 1. Create the resources

```bash
npx wrangler d1 create runbook-production
npx wrangler r2 bucket create runbook-blobs
npx wrangler r2 bucket create runbook-user-content
npx wrangler queues create runbook-notifications
npx wrangler queues create runbook-notifications-dlq
```

## 2. Write the two configs

Copy `wrangler.jsonc.example` to `wrangler.prod.jsonc` and
`wrangler.auth.jsonc.example` to `wrangler.auth.prod.jsonc`; both are
git-ignored. Fill in the D1 ID, the bucket and queue names, and the origins:

- the api Worker's origin variable is `ORIGIN`, the auth Worker's is
  `RUNBOOK_ORIGIN`; set both to the same app origin;
- set `RUNBOOK_USERCONTENT_ORIGIN` on the api Worker to a different
  hostname, preferably on a separate registrable domain;
- uncomment the api config's `AUTH` service and `assets` blocks.

## 3. Build

```bash
node scripts/build-worker.mjs worker
(cd ../auth && node scripts/build.mjs)
(cd ../web && node scripts/build-wasm.mjs && npm run build)
node scripts/prepare-assets.mjs
```

## 4. Apply the D1 migrations before traffic

```bash
npx wrangler d1 migrations apply DB --remote -c wrangler.prod.jsonc
npx wrangler d1 migrations apply DB --remote -c wrangler.auth.prod.jsonc
```

## 5. Deploy

The two Workers bind each other, and a service binding to a Worker that
doesn't exist yet fails the deploy. The first time, deploy in this order:

1. the auth Worker *without* its `services` block (comment it out once);
2. the api Worker: `npx wrangler deploy -c wrangler.prod.jsonc`;
3. the auth Worker again, with `services` restored:
   `npx wrangler deploy -c wrangler.auth.prod.jsonc`.

## 6. Set the secrets

Use `npx wrangler secret put NAME -c <config>`, piping each value in from a
file outside the repository:

- api Worker: `BOOTSTRAP_SECRET`, `WEBHOOK_SECRET_KEY`,
  `RUNBOOK_UPLOAD_SIGNING_KEY` and `RUNBOOK_AUTH_INTERNAL_SECRET`;
- auth Worker: `RUNBOOK_AUTH_SECRET` and the same
  `RUNBOOK_AUTH_INTERNAL_SECRET`.

## 7. Attach the hostnames

Attach the app origin and the user-content origin as Workers custom domains
of the api Worker. Check the zone first: a wildcard DNS record already
answers for every subdomain, and a custom domain takes that name over.

## 8. Create the first owner

Send the bootstrap secret in the `x-bootstrap-secret` header:

```bash
curl -X POST https://runbooks.example.com/internal/bootstrap \
  -H "x-bootstrap-secret: $BOOTSTRAP_SECRET" \
  -H "Content-Type: application/json" \
  -d '{"organization": "My Organization", "user_name": "Initial Admin",
       "project_slug": "default", "owner_email": "admin@example.com"}'
```

The `201` response carries the organization, user and project IDs and a
personal and a service token, shown once. A second call returns `409`
(`bootstrap.already_initialized`). Then remove the secret, which disables
the route:

```bash
npx wrangler secret delete BOOTSTRAP_SECRET
```

## Updating

`npm run update` (from `cloudflare/`) backs up D1 with a Time Travel
bookmark, applies migrations, deploys both Workers, verifies `/v1/meta` and
rolls back the Workers on failure. Start with `npm run update -- --dry-run`.
Until a release is published, use `--source checkout`.
