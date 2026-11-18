import { setupServer } from 'msw/node';
import { handlers } from './handlers';

/** Started for every Vitest file by src/test/setup.ts; unhandled requests fail the test. */
export const server = setupServer(...handlers);
