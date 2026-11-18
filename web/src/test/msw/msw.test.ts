import createClient from 'openapi-fetch';
import { describe, expect, it } from 'vitest';
import type { paths } from '../../api/schema';
import { fixtures } from './fixtures';

// Exercises the generated types + openapi-fetch against the MSW example server.
// openapi-fetch captures `fetch` when the client is created, which is before MSW
// patches it in beforeAll; resolve it per request so interception works.
const client = createClient<paths>({
  baseUrl: 'http://localhost',
  fetch: (request) => globalThis.fetch(request),
});

describe('MSW example server', () => {
  it('serves my-tasks.json for GET /v1/me/tasks', async () => {
    const { data, error } = await client.GET('/v1/me/tasks');
    expect(error).toBeUndefined();
    expect(data?.items.map((t) => t.kind)).toEqual(['approval', 'human_task']);
  });

  it('serves the execution document for GET /v1/executions/{exe}/report', async () => {
    const { data } = await client.GET('/v1/executions/{exe}/report', {
      params: { path: { exe: fixtures.executionDocument.header.id } },
    });
    expect(data).toEqual(fixtures.executionDocument);
  });

  it('serves the 422 problem for POST /v1/executions/{exe}/rerun', async () => {
    const { response, error } = await client.POST('/v1/executions/{exe}/rerun', {
      params: {
        path: { exe: 'exe_01JABCDEFGHJKMNPQRSTVWXYZ0' },
        header: { 'Idempotency-Key': '00000000-0000-4000-8000-000000000000' },
      },
      body: {},
    });
    expect(response.status).toBe(422);
    expect(response.headers.get('content-type')).toContain('application/problem+json');
    expect(error).toMatchObject({ code: 'execution.inputs_invalid' });
  });
});
