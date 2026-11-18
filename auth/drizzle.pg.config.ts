// drizzle-kit migration generation for Postgres (server.ts), `auth` schema only.
import { defineConfig } from 'drizzle-kit';
export default defineConfig({ dialect: 'postgresql', schema: './src/schema/pg.ts', out: './drizzle/pg', schemaFilter: ['auth'] });
