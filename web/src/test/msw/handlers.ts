// Default MSW handlers serving the checked-in API examples. Tests override per case
// with `server.use(...)`. Paths match any origin, so they work with any baseUrl.
// Only implemented operations get default handlers (planned memory routes do not).
import { http, HttpResponse } from 'msw';
import { fixtures } from './fixtures';

export const problem = (body: { status: number }) =>
  HttpResponse.json(body, {
    status: body.status,
    headers: { 'Content-Type': 'application/problem+json' },
  });

export const handlers = [
  http.get('*/v1/me/tasks', () => HttpResponse.json(fixtures.myTasks)),
  http.get('*/v1/executions/:exe/report', () => HttpResponse.json(fixtures.executionDocument)),
  http.get('*/v1/executions/:exe/next', () => HttpResponse.json(fixtures.resumeBrief)),
  http.get('*/v1/executions/:exe', () => HttpResponse.json(fixtures.postDeployCompleted)),
  http.post('*/v1/executions/:exe/rerun', () => problem(fixtures.rerunInputsInvalid)),
  http.get('*/v1/meta', () =>
    HttpResponse.json({
      edition: 'self_hosted',
      version: '0.0.0',
      schema: {},
      capabilities: { uploads: true },
    }),
  ),
  http.get('*/.well-known/runbook', () =>
    HttpResponse.json({
      edition: 'self_hosted',
      api_base_url: 'http://localhost:8080',
      auth_base_url: 'http://localhost:8080/api/auth',
      minimum_cli_version: '0.0.0',
      auth: {
        methods: [],
        device_authorization_endpoint: null,
      },
    }),
  ),
];

