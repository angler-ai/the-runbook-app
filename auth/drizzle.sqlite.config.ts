// drizzle-kit migration generation for D1 (worker.ts). `npm run db:generate`.
import { defineConfig } from 'drizzle-kit';
export default defineConfig({ dialect: 'sqlite', schema: './src/schema/sqlite.ts', out: './drizzle/sqlite' });
