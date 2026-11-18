// Schema-generation configuration only (Shelf's pattern); no real credentials.
// `npx @better-auth/cli@1.7.5 generate --config auth.config.ts --output /tmp/auth-schema.ts`
// prints the Drizzle tables Better Auth expects for exactly this option set (both
// methods on). Fold any change into src/schema/{sqlite,pg}.ts, keeping the `auth_`
// prefix on SQLite and the `auth` schema on Postgres, then `npm run db:generate`.
// tests/schema.test.mjs fails if the two schemas drift from what Better Auth needs.
import { drizzleAdapter } from '@better-auth/drizzle-adapter';
import { drizzle } from 'drizzle-orm/postgres-js';
import postgres from 'postgres';
import { createAuth } from './src/app';
import { loadConfig } from './src/config';
import { drizzleStore } from './src/limits';
import * as schema from './src/schema/pg';

const config = loadConfig(
  {
    RUNBOOK_AUTH_INTERNAL_SECRET: 'schema-generation-only-internal-secret-0000',
    RUNBOOK_AUTH_SECRET: 'schema-generation-only-not-a-production-secret',
    RUNBOOK_ORIGIN: 'https://runbook.example.test',
    RUNBOOK_AUTH_CAN_REGISTER_URL: 'http://127.0.0.1:1/internal/auth/can-register',
    RUNBOOK_SMTP_URL: 'smtp://127.0.0.1:1',
    RUNBOOK_SMTP_FROM: 'login@example.test',
    RUNBOOK_GOOGLE_CLIENT_ID: 'schema-generation-only',
    RUNBOOK_GOOGLE_CLIENT_SECRET: 'schema-generation-only',
  },
  'server',
);
const db = drizzle(postgres('postgres://schema-generation-only@127.0.0.1:1/none', { max: 1 }), { schema });
export const auth = createAuth({
  config,
  database: drizzleAdapter(db, { provider: 'pg', schema }),
  store: drizzleStore(db, schema),
  transport: { async send() {} },
  clientIpHeader: 'x-runbook-client-ip',
  fetch: () => Promise.reject(new Error('schema generation makes no requests')),
});
