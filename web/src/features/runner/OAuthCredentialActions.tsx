import { oauthAuthorizationUrl } from '../../../../runner/packages/shared/src/oauth-binding';
import { useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Input } from '../../ui/primitives/input';
import { Field } from './presentation';
import { validateSecret } from './credentialForm';
import { Button } from '../../ui/primitives/button';
import { saveCredentialSecrets, verifyVaultSignature } from './seal';
import { runnerError, runnerRequest, runnerVaultIdentity, type Row } from './api';
import type { Credential } from './Credentials';
import { credentialBindingHash, destinationBinding } from './destination-binding';

interface OAuthReturn {
  id: string;
  result?: string;
  pin?: string;
  user?: string;
  identity?: string;
  authorizationHandoff?: string;
}

export function oauthReturn(): OAuthReturn | undefined {
  const fragment = window.location.hash;
  const result =
    /^#oauth-result=([A-Za-z0-9_-]{1,128})\.(connected|confirm-account|test-failed|failed)$/.exec(
      fragment,
    );
  if (result) return { id: result[1]!, result: result[2] };
  const request =
    /^#oauth-connect=([A-Za-z0-9_-]{1,128})\.([A-Za-z0-9_-]{43})\.([A-Za-z0-9_-]{1,128})\.([A-Za-z0-9_-]{43})\.([A-Za-z0-9_-]{43})$/.exec(
      fragment,
    );
  return request
    ? {
        id: request[1]!,
        pin: request[2],
        user: request[3],
        identity: request[4],
        authorizationHandoff: request[5],
      }
    : undefined;
}

type Discovery = {
  config: Row;
  groups: Credential['groups'];
  confirmed: boolean;
  registration_pending?: boolean;
};

function oauthHost(value: unknown): string {
  try {
    return typeof value === 'string' ? new URL(value).hostname : '';
  } catch {
    return '';
  }
}

const destinations: [string, string][] = [
  ['issuer', 'Issuer'],
  ['resource', 'Resource'],
  ['authorization_endpoint', 'Authorization endpoint'],
  ['token_endpoint', 'Token endpoint'],
  ['revocation_endpoint', 'Revocation endpoint'],
  ['registration_endpoint', 'Registration endpoint'],
  ['redirect_uri', 'Redirect URI'],
];

export function OAuthCredentialActions({
  credential,
  install,
  org,
  disabled,
  handoff,
  impact,
  notify,
  errors,
}: {
  credential: Credential;
  install: string;
  org: string;
  disabled: boolean;
  handoff?: OAuthReturn;
  impact?: string;
  notify(message: string): void;
  errors(message: string): void;
}) {
  const client = useQueryClient();
  const [discovered, setDiscovered] = useState<Discovery>();
  const secretInput = useRef<HTMLInputElement>(null);
  const pendingDiscovery = discovered ?? credential.oauth_status?.discovery_candidate;
  const [busy, setBusy] = useState(false);
  const [disconnect, setDisconnect] = useState(false);
  const config = pendingDiscovery?.config ?? credential.config;
  const confirmed = pendingDiscovery?.confirmed ?? Boolean(credential.config.issuer);
  const path = `/credentials/${credential.id}`;
  const info = credential.oauth_status;
  const needsSecret =
    confirmed &&
    config.registration_mode === 'pre-registered' &&
    config.token_endpoint_auth_method !== 'none' &&
    !credential.secret_meta?.client_secret;
  const requested = Array.isArray(config.scopes) ? config.scopes.map(String) : [];

  const pending = info?.account_confirmation;
  const awaitingConfirmation = Boolean(pending);
  const accountProof = useQuery({
    queryKey: [
      'oauth-account',
      org,
      install,
      credential.id,
      credential.config,
      credential.groups,
      pending,
      info?.pending_account,
      info?.account_changed,
      info?.account_confirmation_signature,
      info?.account_confirmation_expires_at_ms,
    ],
    enabled: awaitingConfirmation,
    retry: false,
    queryFn: async () => {
      const account = info?.pending_account;
      const changed = info?.account_changed;
      const signature = info?.account_confirmation_signature;
      const expires = info?.account_confirmation_expires_at_ms;
      if (
        !pending ||
        !/^[A-Za-z0-9_-]{43}$/.test(pending) ||
        (account !== null && (typeof account !== 'string' || !account)) ||
        typeof changed !== 'boolean' ||
        typeof signature !== 'string' ||
        !expires ||
        !Number.isSafeInteger(expires) ||
        expires <= Date.now()
      ) {
        throw new Error('Could not verify the vault account confirmation.');
      }
      const binding = await credentialBindingHash(credential);
      const identity = await runnerVaultIdentity(org, install);
      await verifyVaultSignature(identity, signature, [
        'runbook-runner/oauth-account/v1',
        org,
        install,
        credential.id,
        binding,
        pending,
        account ?? '',
        String(changed),
        String(expires),
      ]);
      return { account, changed, expires, binding, pending };
    },
  });
  const verifiedAccount =
    awaitingConfirmation && !accountProof.isError ? accountProof.data : undefined;
  const issuerHost = oauthHost(config.issuer);
  const serverHost = oauthHost(config.url);

  async function operation(action: () => Promise<void>) {
    setBusy(true);
    notify('');
    errors('');
    try {
      await action();
      await client.invalidateQueries({ queryKey: ['runner-proxy', install] });
    } catch (error) {
      errors(runnerError(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="mt-4 grid gap-3" aria-label="OAuth connection">
      {credential.status === 'reconnect_required' && info?.reconnect_reason && (
        <p role="status">Reconnect required ({info.reconnect_reason.replaceAll('_', ' ')}).</p>
      )}
      {credential.status === 'active' && info?.refresh_failure_since && (
        <p role="status">
          Token refresh has been failing since{' '}
          {new Date(info.refresh_failure_since).toLocaleString()}.
        </p>
      )}
      {info?.purge_until && (
        <p role="status">
          Pending gateway purge. Released access may last until{' '}
          {new Date(info.purge_until).toLocaleString()}.
        </p>
      )}
      {awaitingConfirmation && accountProof.isError && (
        <p role="status">Could not verify the vault account confirmation.</p>
      )}
      {verifiedAccount && (
        <div className="grid gap-2">
          <p>
            {verifiedAccount.account
              ? `Provider account ${verifiedAccount.account}. Confirm that this is the account you intended to connect.`
              : 'Account not reported by the provider.'}
          </p>
          {verifiedAccount.changed && (
            <p role="status">This account differs from the previously connected account.</p>
          )}
          <Button
            className="w-fit"
            disabled={disabled || busy}
            onClick={() =>
              void operation(async () => {
                if (verifiedAccount.expires <= Date.now()) {
                  throw new Error('The account confirmation expired. Restart authorization.');
                }
                const result = await runnerRequest<{ connected: boolean; test?: { ok: boolean } }>(
                  install,
                  `${path}/oauth-confirm`,
                  'POST',
                  {
                    binding_hash: verifiedAccount.binding,
                    confirm_account: verifiedAccount.pending,
                  },
                );
                if (result.connected)
                  notify(
                    result.test?.ok === false
                      ? 'OAuth connected. The MCP connection test failed.'
                      : 'OAuth connection passed.',
                  );
              })
            }
          >
            {verifiedAccount.account
              ? 'Confirm account and connect'
              : 'Confirm without a reported account'}
          </Button>
        </div>
      )}
      {credential.status === 'active' && (
        <p className="text-sm">
          Connected account {info?.connected_account ?? 'unavailable from provider'}
        </p>
      )}
      {info?.granted_scopes && (
        <p className="text-sm">Granted scopes {info.granted_scopes.join(', ') || 'none'}</p>
      )}
      {info?.granted_scopes &&
        requested.length > 0 &&
        (requested.some((scope) => !info.granted_scopes?.includes(scope)) ||
          info.granted_scopes.length !== requested.length) && (
          <p className="text-sm">Requested scopes {requested.join(', ')}</p>
        )}
      {info?.grant_expires_at && (
        <p className="text-sm">
          Connection expires {new Date(info.grant_expires_at).toLocaleDateString()}
        </p>
      )}
      {info?.access_only && (
        <p className="text-sm">
          The provider issued an access-only grant. Reconnect when it expires.
        </p>
      )}
      {info?.expiry_warning && credential.status === 'active' && (
        <p role="status" className="text-warning">
          This OAuth grant expires within 14 days. Reconnect to keep using it.
        </p>
      )}
      {issuerHost && (
        <p className="text-sm">
          Sign in at <strong>{oauthHost(config.authorization_endpoint) || issuerHost}</strong>. This
          grants access to {serverHost || 'the MCP server'}.
        </p>
      )}
      {issuerHost &&
        serverHost &&
        [issuerHost, oauthHost(config.authorization_endpoint)].some(
          (host) =>
            host &&
            host !== serverHost &&
            !(serverHost === 'mcp.notion.com' && host === 'api.notion.com'),
        ) && (
          <p role="status">
            Check this sign-in domain with your workspace administrator before continuing.
          </p>
        )}
      {pendingDiscovery && !confirmed && (
        <p className="text-sm text-fg-muted">
          Confirm that you trust the sign-in domain for this server before continuing.
        </p>
      )}
      {Boolean(config.issuer) && (
        <details className="rounded-md border border-border p-3">
          <summary className="cursor-pointer font-medium">Advanced endpoint details</summary>
          <dl className="mt-2 grid gap-1 text-sm">
            {destinations
              .filter(([key]) => config[key])
              .map(([key, label]) => (
                <div key={key} className="grid gap-1">
                  <dt className="font-medium">{label}</dt>
                  <dd className="break-all text-fg-muted">{String(config[key])}</dd>
                </div>
              ))}
          </dl>
        </details>
      )}
      {handoff?.pin && (
        <p className="text-sm">
          Confirm this connection in the same account that started it in the CLI.
        </p>
      )}
      {needsSecret && !disabled && !handoff && (
        <Field label="OAuth client secret">
          <Input ref={secretInput} type="password" autoComplete="new-password" />
        </Field>
      )}
      <div className="flex flex-wrap gap-2">
        {!confirmed && !pendingDiscovery?.registration_pending && (
          <Button
            variant="secondary"
            disabled={disabled || busy}
            onClick={() =>
              void operation(async () => {
                setDiscovered(
                  await runnerRequest<Discovery>(install, `${path}/oauth-discover`, 'POST', {}),
                );
              })
            }
          >
            Discover OAuth endpoints
          </Button>
        )}
        {pendingDiscovery && !confirmed && (
          <Button
            disabled={disabled || busy}
            onClick={() =>
              void operation(async () => {
                const binding_hash = await credentialBindingHash({
                  type: credential.type,
                  ...pendingDiscovery,
                });
                setDiscovered(
                  await runnerRequest<Discovery>(install, `${path}/oauth-confirm`, 'POST', {
                    binding_hash,
                    ...(pendingDiscovery.registration_pending && { reset_registration: true }),
                  }),
                );
              })
            }
          >
            {pendingDiscovery.registration_pending
              ? 'Reset pending registration and retry'
              : 'Confirm endpoints'}
          </Button>
        )}
        {confirmed && !info?.purge_until && (
          <Button
            disabled={disabled || busy}
            onClick={() =>
              void operation(async () => {
                const confirmedDestination = {
                  type: credential.type,
                  config,
                  groups: pendingDiscovery?.groups ?? credential.groups,
                };
                const binding_hash = await credentialBindingHash(confirmedDestination);
                if (handoff?.pin && binding_hash !== handoff.pin) {
                  errors('The OAuth binding changed since the CLI request.');
                  return;
                }
                const identity = await runnerVaultIdentity(org, install);
                if (handoff?.identity && handoff.identity !== identity) {
                  throw new Error('The vault identity changed since the CLI request.');
                }
                if (needsSecret && secretInput.current) {
                  const value = secretInput.current.value;
                  secretInput.current.value = '';
                  validateSecret(value, 'OAuth client secret');
                  await saveCredentialSecrets(
                    (route, method, body) => runnerRequest(install, route, method, body),
                    org,
                    { ...credential, ...confirmedDestination },
                    { client_secret: value },
                    { installId: install, identityKey: identity },
                  );
                }
                const result = await runnerRequest<{
                  authorization_url: string;
                  identity_signature: string;
                  expires_at_ms: number;
                  nonce_hash: string;
                  session_hash: string;
                  request_id: string;
                }>(install, `${path}/connect`, 'POST', {
                  binding_hash,
                  ...(handoff?.user && { initiating_user: handoff.user }),
                  ...(handoff?.authorizationHandoff && {
                    authorization_handoff: handoff.authorizationHandoff,
                  }),
                });
                try {
                  if (
                    !Number.isSafeInteger(result.expires_at_ms) ||
                    result.expires_at_ms <= Date.now()
                  ) {
                    throw new Error();
                  }
                  await verifyVaultSignature(identity, result.identity_signature, [
                    'runbook-runner/oauth-start/v1',
                    org,
                    install,
                    credential.id,
                    binding_hash,
                    result.authorization_url,
                    result.nonce_hash,
                    result.session_hash,
                    result.request_id,
                    String(result.expires_at_ms),
                    handoff?.authorizationHandoff ?? '',
                  ]);
                } catch {
                  throw new Error('Could not verify the vault authorization.');
                }
                const authorization = new URL(result.authorization_url);
                const expected = oauthAuthorizationUrl(
                  destinationBinding(confirmedDestination),
                  authorization.searchParams.get('state') ?? '',
                  authorization.searchParams.get('code_challenge') ?? '',
                );
                if (authorization.href !== expected) {
                  throw new Error('The authorization URL did not match the confirmed destination.');
                }
                if (handoff?.authorizationHandoff) {
                  notify('Open the authorization URL printed by the CLI in this browser.');
                } else {
                  window.location.assign(authorization.href);
                }
              })
            }
          >
            {awaitingConfirmation
              ? 'Restart authorization'
              : credential.status === 'active' || credential.status === 'reconnect_required'
                ? 'Reconnect'
                : 'Connect'}
          </Button>
        )}
        {(credential.status === 'active' ||
          credential.status === 'reconnect_required' ||
          info?.purge_until ||
          awaitingConfirmation) && (
          <Button
            variant="secondary"
            disabled={disabled || busy}
            onClick={() => setDisconnect(true)}
          >
            Disconnect
          </Button>
        )}
        {disconnect && (
          <div
            role="group"
            aria-label="Disconnect impact"
            className="grid w-full gap-3 rounded-md border border-border p-3"
          >
            <div>
              <p className="font-medium">
                Disconnect {credential.name}
                {info?.connected_account ? ` from ${info.connected_account}` : ''}?
              </p>
              <p className="text-sm text-fg-muted">
                Project access:{' '}
                {impact ??
                  (Array.isArray(credential.sharing.projects)
                    ? `${credential.sharing.projects.length} selected projects`
                    : credential.sharing.projects === 'all'
                      ? 'All projects'
                      : 'Private')}
                . Future calls that need this sign-in will fail. Revoke access at the provider if it
                cannot be revoked here.
              </p>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="destructive"
                disabled={disabled || busy}
                onClick={() =>
                  void operation(async () => {
                    const result = await runnerRequest<{
                      revoked: boolean;
                      pending_purge: boolean;
                      access_token_remaining_ms: number;
                    }>(install, `${path}/disconnect`, 'POST', {});
                    setDisconnect(false);
                    notify(
                      result.pending_purge
                        ? `Pending gateway purge. Released access may last up to ${Math.ceil(result.access_token_remaining_ms / 60_000)} more minutes.`
                        : result.revoked
                          ? 'Disconnected.'
                          : 'Disconnected locally. Revoke the grant at the provider.',
                    );
                  })
                }
              >
                Confirm disconnect
              </Button>
              <Button variant="secondary" disabled={busy} onClick={() => setDisconnect(false)}>
                Cancel
              </Button>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}
