// @vitest-environment jsdom
import type * as sealModule from './seal';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { Credentials } from './Credentials';
import { readSource, formDestination } from './credentialForm';
import { CREDENTIAL_TYPES } from '../../../../runner/packages/shared/src/credential-manifest';
import { destinationBinding } from './destination-binding';

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
  sharing: { projects: 'none' as const },
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
  identity.mockReset().mockResolvedValue('trusted-key');
  request.mockReset();
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

it('flags an OAuth issuer on a different host before confirmation', async () => {
  const { OAuthCredentialActions } = await import('./OAuthCredentialActions');
  request.mockResolvedValueOnce({
    confirmed: false,
    groups: 'all',
    config: { url: 'https://resources.example/mcp', issuer: 'https://issuer.example' },
  });
  render(
    <QueryClientProvider client={new QueryClient()}>
      <OAuthCredentialActions
        credential={{
          ...row,
          type: 'mcp-oauth',
          groups: 'all',
          config: { url: 'https://resources.example/mcp' },
        }}
        install="install"
        org="org"
        disabled={false}
        notify={vi.fn()}
        errors={vi.fn()}
      />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: 'Discover OAuth endpoints' }));
  expect(
    await screen.findByText(
      /OAuth issuer \(issuer\.example\) differs from the MCP server host \(resources\.example\)/,
    ),
  ).toBeVisible();
  expect(screen.getByRole('button', { name: 'Confirm endpoints' })).toBeEnabled();
});

const oauthRow = {
  ...row,
  type: 'mcp-oauth',
  config: {
    url: 'https://mcp.example',
    issuer: 'https://auth.example',
    resource: 'https://mcp.example',
    registration_mode: 'pre-registered',
    client_id: 'client',
    token_endpoint_auth_method: 'none',
    iss_supported: false,
    authorization_endpoint: 'https://auth.example/authorize',
    token_endpoint: 'https://auth.example/token',
    redirect_uri: 'https://runbook.example/v1/runner/oauth/callback/opened',
  },
};

async function signFixture(fields: string[]) {
  const { lengthPrefixed, toBase64Url } = await import('./encoding');
  const key = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
  identity.mockResolvedValueOnce(
    toBase64Url(new Uint8Array(await crypto.subtle.exportKey('raw', key.publicKey))),
  );
  return toBase64Url(
    new Uint8Array(
      await crypto.subtle.sign('Ed25519', key.privateKey, new Uint8Array(lengthPrefixed(fields))),
    ),
  );
}

it.each([false, true])(
  'verifies the vault account display before enabling confirmation (changed content: %s)',
  async (tampered) => {
    const { OAuthCredentialActions } = await import('./OAuthCredentialActions');
    const { credentialBindingHash } = await import('./destination-binding');
    const binding = await credentialBindingHash(oauthRow);
    const expires = Date.now() + 60_000;
    const operation = 'A'.repeat(43);
    const signature = await signFixture([
      'runbook-runner/oauth-account/v1',
      'org',
      'install',
      row.id,
      binding,
      operation,
      '',
      'false',
      String(expires),
    ]);
    render(
      <QueryClientProvider client={new QueryClient()}>
        <OAuthCredentialActions
          credential={{
            ...oauthRow,
            oauth_status: {
              account_confirmation: operation,
              account_confirmation_signature: signature,
              account_confirmation_expires_at_ms: expires,
              account_changed: false,
              pending_account: tampered ? 'Unverified label' : null,
            },
          }}
          install="install"
          org="org"
          disabled={false}
          notify={vi.fn()}
          errors={vi.fn()}
        />
      </QueryClientProvider>,
    );
    if (tampered) {
      expect(
        await screen.findByText('Could not verify the vault account confirmation.'),
      ).toBeVisible();
      expect(screen.queryByText(/Unverified label/)).toBeNull();
      expect(screen.queryByRole('button', { name: 'Confirm account and connect' })).toBeNull();
    } else {
      expect(await screen.findByText(/Account not reported by the provider/)).toBeVisible();
      expect(request).not.toHaveBeenCalled();
      request.mockResolvedValueOnce({ connected: true });
      fireEvent.click(screen.getByRole('button', { name: 'Confirm without a reported account' }));
      await waitFor(() =>
        expect(request).toHaveBeenCalledWith(
          'install',
          '/credentials/opened/oauth-confirm',
          'POST',
          { binding_hash: binding, confirm_account: operation },
        ),
      );
    }
  },
);

it.each([false, true])(
  'verifies the vault signature on an OAuth start (changed signature: %s)',
  async (tampered) => {
    const { OAuthCredentialActions } = await import('./OAuthCredentialActions');
    const { credentialBindingHash, destinationBinding } = await import('./destination-binding');
    const { oauthAuthorizationUrl } =
      await import('../../../../runner/packages/shared/src/oauth-binding');
    const binding = await credentialBindingHash(oauthRow);
    const authorization = oauthAuthorizationUrl(
      destinationBinding(oauthRow),
      'b'.repeat(43),
      'c'.repeat(43),
    );
    const expires = Date.now() + 60_000;
    const handoff = 'd'.repeat(43);
    const signature = await signFixture([
      'runbook-runner/oauth-start/v1',
      'org',
      'install',
      row.id,
      binding,
      authorization,
      'nonce',
      'session',
      'request',
      String(expires),
      tampered ? 'e'.repeat(43) : handoff,
    ]);
    request.mockResolvedValueOnce({
      authorization_url: authorization,
      identity_signature: signature,
      expires_at_ms: expires,
      nonce_hash: 'nonce',
      session_hash: 'session',
      request_id: 'request',
    });
    const errors = vi.fn();
    const notify = vi.fn();
    render(
      <QueryClientProvider client={new QueryClient()}>
        <OAuthCredentialActions
          credential={oauthRow}
          handoff={{ id: row.id, authorizationHandoff: handoff }}
          install="install"
          org="org"
          disabled={false}
          notify={notify}
          errors={errors}
        />
      </QueryClientProvider>,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
    if (tampered) {
      await waitFor(() =>
        expect(errors).toHaveBeenCalledWith('Could not verify the vault authorization.'),
      );
    } else {
      await waitFor(() =>
        expect(notify).toHaveBeenCalledWith(
          'Open the authorization URL printed by the CLI in this browser.',
        ),
      );
    }
  },
);

it('validates and seals a Secret Manager reference against its resource and version', async () => {
  open();
  const source = {
    type: 'gcp-secret-manager',
    resource: 'projects/sample/secrets/mcp',
    version: 'latest',
  };
  fireEvent.change(screen.getByLabelText('Secret source'), { target: { value: source.type } });
  fireEvent.change(screen.getByLabelText('Secret resource name'), {
    target: { value: source.resource },
  });
  fireEvent.change(screen.getByLabelText('Secret version'), { target: { value: '0' } });
  const form = screen.getByRole('button', { name: 'Save credential' }).closest('form')!;
  fireEvent.submit(form);
  expect(await screen.findByRole('alert')).toHaveTextContent(/Secret version.*positive/);
  expect(request).not.toHaveBeenCalled();
  fireEvent.change(screen.getByLabelText('Secret version'), { target: { value: source.version } });
  request.mockResolvedValueOnce({ ...row, config: { ...row.config, secret_source: source } });
  fireEvent.submit(form);
  await waitFor(() =>
    expect(seal).toHaveBeenCalledWith(
      expect.any(Function),
      'org',
      expect.objectContaining({ config: { ...row.config, secret_source: source } }),
      {
        token:
          '{"resource":"projects/sample/secrets/mcp","type":"gcp-secret-manager","version":"latest"}',
      },
      { installId: 'install', identityKey: 'trusted-key' },
    ),
  );
  expect(
    destinationBinding({ ...row, config: { ...row.config, secret_source: source } }),
  ).toHaveProperty('secret_source', source);
});

it('names the rule when a dynamic client tries to set manual authentication', () => {
  const fields = new FormData();
  fields.set('url', 'https://mcp.example/mcp');
  fields.set('registration_mode', 'dynamic');
  fields.set('token_endpoint_auth_method', 'none');
  expect(() => formDestination(fields, CREDENTIAL_TYPES['mcp-oauth'])).toThrow(
    /client authentication are decided by registration/i,
  );
  fields.set('secret-resource', 'not-a-resource');
  expect(() => readSource(fields)).toThrow(/Secret resource name must use projects/);
});
