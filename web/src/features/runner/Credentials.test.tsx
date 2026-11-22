// @vitest-environment jsdom
import type * as sealModule from './seal';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { Credentials } from './Credentials';

const { request, identity, seal } = vi.hoisted(() => ({
  request: vi.fn(),
  identity: vi.fn(async () => 'trusted-key'),
  seal: vi.fn(),
}));
const row = {
  id: 'opened',
  name: 'First credential',
  type: 'mcp-bearer',
  status: 'active',
  delivery: ['mcp'],
  label: 'destructive',
  groups: 'all',
  sharing: { projects: 'none' },
  config: { url: 'https://mcp.example' },
  secret_meta: {},
};
vi.mock('./api', () => ({
  useRunnerQuery: (_status: unknown, path: string) => ({
    data: {
      items: path.includes('presets')
        ? [
            {
              type: 'mcp-bearer',
              title: 'Remote preset',
              test: '',
              configFields: [{ name: 'token', label: 'Fake metadata', kind: 'text' }],
              secretFields: [{ name: 'token', label: 'Token' }],
            },
          ]
        : [row],
    },
  }),
  useRunnerMutation: () => ({ isPending: false }),
  runnerRequest: request,
  runnerVaultIdentity: identity,
  runnerError: (error: Error) => error.message,
}));
vi.mock('./seal', async (original) => ({
  ...(await original<typeof sealModule>()),
  saveCredentialSecrets: seal,
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function open(existing = true) {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <Credentials
        status={{ status: 'online', install_id: 'install', groups: [], runners: [] }}
        org="org"
        editable
      />
    </QueryClientProvider>,
  );
  if (existing) fireEvent.click(screen.getByRole('button', { name: row.name }));
}

it('classifies credential fields using the browser manifest', () => {
  open();
  expect(screen.queryByLabelText('Fake metadata')).toBeNull();
  expect(screen.getByLabelText('MCP server URL')).toHaveValue('https://mcp.example');
  expect(screen.getByLabelText('Replace token (optional)')).toHaveAttribute('type', 'password');
});

it('refuses to send a secret to a different credential ID returned from a rotation update', async () => {
  open();
  request.mockResolvedValueOnce({ ...row, id: 'sibling' });
  fireEvent.change(screen.getByLabelText('Replace token (optional)'), {
    target: { value: 'replacement' },
  });
  fireEvent.submit(screen.getByRole('button', { name: 'Save credential' }).closest('form')!);
  expect(await screen.findByRole('alert')).toHaveTextContent(/credential.*changed/i);
  expect(request).toHaveBeenCalledWith(
    'install',
    '/credentials/opened',
    'PATCH',
    expect.anything(),
  );
  expect(request.mock.calls[0]?.[3]).not.toHaveProperty('label');
  expect(JSON.stringify(request.mock.calls)).not.toContain('replacement');
  expect(seal).not.toHaveBeenCalled();
});

it('requires a vault receipt before sealing to the ID from a create reply', async () => {
  open(false);
  request.mockResolvedValueOnce(row);
  fireEvent.click(screen.getByRole('button', { name: 'Add credential' }));
  fireEvent.change(screen.getByLabelText('Credential type'), { target: { value: 'mcp-bearer' } });
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'New credential' } });
  fireEvent.change(screen.getByLabelText('MCP server URL'), {
    target: { value: 'https://mcp.example' },
  });
  fireEvent.change(screen.getByLabelText(/token/i, { selector: 'input[type="password"]' }), {
    target: { value: 'new-secret' },
  });
  fireEvent.submit(screen.getByRole('button', { name: 'Save credential' }).closest('form')!);
  expect(await screen.findByRole('alert')).toHaveTextContent(/vault credential receipt/i);
  expect(seal).not.toHaveBeenCalled();
});
