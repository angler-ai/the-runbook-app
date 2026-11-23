import { expect, test, type Page } from '@playwright/test';
import { generateKeyPairSync, sign } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { oauthAuthorizationUrl } from '../../../runner/packages/shared/src/oauth-binding.ts';
import {
  credentialBindingHash,
  destinationBinding,
} from '../../src/features/runner/destination-binding.ts';
import { lengthPrefixed } from '../../src/features/runner/encoding.ts';
import * as fixture from './fixtures.ts';

const shots =
  process.env.RUNNER_UI_SHOTS ??
  fileURLToPath(new URL('../../../../.reviews/v011-runner-prd/ui-shots', import.meta.url));
const local = 'http://127.0.0.1:5186';
const provider = 'https://oauth.example.test';
const credentialId = 'notion-fixture';
const connectionId = 'notion-connection';

async function mockRunner(page: Page) {
  const keys = generateKeyPairSync('ed25519');
  const identity = keys.publicKey
    .export({ format: 'der', type: 'spki' })
    .subarray(-32)
    .toString('base64url');
  const signature = (fields: string[]) =>
    sign(null, lengthPrefixed(fields), keys.privateKey).toString('base64url');
  const connections = structuredClone(fixture.existingConnections);
  let credential: Record<string, unknown> | undefined;
  let confirmed: Record<string, unknown> | undefined;
  let connects = 0;
  let notionDiscovery: typeof fixture.warehouseDiscovery | undefined;
  const pinned = { ...fixture.pinnedSnapshots };
  const groups = 'all' as const;
  const type = 'mcp-oauth';
  const account = 'Northstar Operations';
  const selected = () => connections.find((item) => item.id === connectionId)!;
  await page.route(`${provider}/**`, async (route) => {
    expect(new URL(route.request().url()).pathname).toBe('/authorize');
    await route.fulfill({
      contentType: 'text/html',
      body: `<!doctype html><meta charset="utf-8"><title>Mock Notion</title><h1>Authorize workspace access</h1><a href="${local}/settings/organization/runner?tab=credentials#oauth-result=${credentialId}.confirm-account">Allow access</a>`,
    });
  });
  await page.route('**/v1/**', async (route) => {
    const url = new URL(route.request().url());
    const method = route.request().method();
    const json = (body: unknown) => route.fulfill({ json: body });
    const list = (items: unknown[]) => json({ items });
    const data = () => route.request().postDataJSON() as Record<string, unknown>;
    const proxy = url.pathname.split('/proxy/runner/v1')[1];
    if (proxy === undefined) {
      switch (url.pathname) {
        case '/v1/meta':
          return json({ edition: 'self_hosted', version: '0.1.1', schema: {}, capabilities: {} });
        case '/v1/me':
          return json(fixture.fixtureMe);
        case '/v1/orgs':
          return list([{ id: fixture.org, name: 'Northstar', slug: 'northstar' }]);
        case `/v1/orgs/${fixture.org}/runner`:
          return json({ ...fixture.fixtureStatus, vault_identity_key: identity });
        case `/v1/orgs/${fixture.org}/projects`:
          return list([fixture.fixtureProject]);
        default:
          return list([]);
      }
    }
    switch (proxy) {
      case '/credentials':
        if (method === 'POST') {
          const input = data() as {
            config: Record<string, unknown>;
            name: string;
            label: string;
            sharing: { projects: string };
          };
          const binding = await credentialBindingHash({ type, groups, config: input.config });
          credential = {
            ...input,
            id: credentialId,
            groups,
            type,
            status: 'needs_connect',
            delivery: ['mcp'],
            secret_meta: {},
            creation_receipt: signature([
              'runbook-runner/credential-create/v1',
              fixture.org,
              fixture.install,
              credentialId,
              input.name,
              binding,
              input.label,
              JSON.stringify(input.sharing.projects),
            ]),
          };
          return json(credential);
        }
        return list([fixture.credentialRow, ...(credential ? [credential] : [])]);
      case `/credentials/${credentialId}`:
        if (method === 'PATCH') credential = { ...credential, ...data() };
        return json(credential);
      case `/credentials/${credentialId}/oauth-discover`: {
        const config = {
          ...(credential!.config as object),
          resource: 'https://mcp.notion.com/mcp',
          issuer: provider,
          iss_supported: false,
          authorization_endpoint: `${provider}/authorize`,
          token_endpoint: `${provider}/token`,
          registration_endpoint: `${provider}/register`,
          redirect_uri: `${provider}/callback`,
          token_endpoint_auth_method: 'none',
        };
        const candidate = { config, groups, confirmed: false };
        credential = { ...credential, oauth_status: { discovery_candidate: candidate } };
        return json(candidate);
      }
      case `/credentials/${credentialId}/oauth-confirm`: {
        if (data().confirm_account) {
          expect(data().confirm_account).toBe('p'.repeat(43));
          credential = {
            ...credential,
            status: 'active',
            oauth_status: { connected_account: account },
          };
          return json({ connected: true, test: { ok: true } });
        }
        const candidate = (
          credential!.oauth_status as { discovery_candidate: { config: Record<string, unknown> } }
        ).discovery_candidate;
        expect(data().binding_hash).toBe(
          await credentialBindingHash({ type, groups, config: candidate.config }),
        );
        const config = { ...candidate.config, client_id: 'notion-test-client' };
        credential = { ...credential, config, oauth_status: {} };
        confirmed = config;
        return json({ config, groups, confirmed: true });
      }
      case `/credentials/${credentialId}/connect`: {
        connects++;
        const binding = await credentialBindingHash({ type, groups, config: confirmed! });
        expect(data().binding_hash).toBe(binding);
        const authorization_url = oauthAuthorizationUrl(
          destinationBinding({ type, groups, config: confirmed! }),
          's'.repeat(43),
          'c'.repeat(43),
        );
        const expires_at_ms = Date.now() + 120_000;
        const nonce_hash = 'fixture-nonce';
        const session_hash = 'fixture-session';
        const request_id = 'fixture-request';
        const pending = 'p'.repeat(43);
        credential = {
          ...credential,
          oauth_status: {
            pending_account: account,
            account_changed: false,
            account_confirmation: pending,
            account_confirmation_expires_at_ms: expires_at_ms,
            account_confirmation_signature: signature([
              'runbook-runner/oauth-account/v1',
              fixture.org,
              fixture.install,
              credentialId,
              binding,
              pending,
              account,
              'false',
              String(expires_at_ms),
            ]),
          },
        };
        return json({
          authorization_url,
          expires_at_ms,
          nonce_hash,
          session_hash,
          request_id,
          identity_signature: signature([
            'runbook-runner/oauth-start/v1',
            fixture.org,
            fixture.install,
            credentialId,
            binding,
            authorization_url,
            nonce_hash,
            session_hash,
            request_id,
            String(expires_at_ms),
            '',
          ]),
        });
      }
      case '/mcp-servers':
        if (method === 'POST') {
          connections.push({ ...data(), id: connectionId } as ConnectionFromFixture);
          return json(selected());
        }
        return list(connections);
      case `/mcp-servers/${connectionId}`:
        if (method === 'PATCH') Object.assign(selected(), data());
        return json(selected());
      case `/mcp-servers/${connectionId}/discover`:
        expect(connects).toBe(1);
        expect(credential?.status).toBe('active');
        notionDiscovery = { snapshot_id: 'notion_pin', hash: 'fixture-hash', tools: fixture.tools };
        return json(notionDiscovery);
      case `/mcp-servers/${connectionId}/pin`: {
        expect(data()).toEqual({
          snapshot_id: 'notion_pin',
          tool_labels: { search: 'read' },
          never_allowed: ['update_page'],
        });
        const { tool_labels, never_allowed } = data();
        Object.assign(selected(), {
          tool_labels,
          never_allowed,
          pinned_snapshot_id: 'notion_pin',
          pinned_tool_count: fixture.tools.length,
        });
        pinned[connectionId] = notionDiscovery!;
        return json(selected());
      }
      case `/mcp-servers/${connectionId}/tools`:
        return json({ snapshot: pinned[connectionId] ?? null });
      case `/mcp-servers/${connectionId}/drift`:
        return json({
          latest: notionDiscovery ?? null,
          changes: {
            added:
              notionDiscovery && !pinned[connectionId] ? fixture.tools.map(({ name }) => name) : [],
            changed: [],
            removed: [],
          },
        });
      case '/mcp-servers/warehouse':
        return json(connections.find(({ id }) => id === 'warehouse'));
      case '/mcp-servers/warehouse/tools':
        return json({ snapshot: pinned.warehouse });
      case '/mcp-servers/warehouse/drift':
        return json({ latest: fixture.warehouseDiscovery, changes: fixture.warehouseChanges });
      case `/projects/${fixture.project.id}/mcp-tools`:
        return list(
          connections
            .filter(
              (connection) =>
                connection.sharing.projects === 'all' ||
                (Array.isArray(connection.sharing.projects) &&
                  connection.sharing.projects.includes(fixture.project.id)),
            )
            .map(({ id, name, pinned_snapshot_id, never_allowed, tool_labels }) => ({
              id,
              name,
              pinned_snapshot_id,
              tools:
                pinned[id]?.tools
                  .filter((tool) => !never_allowed?.includes(tool.name))
                  .map((tool) => ({
                    name: tool.name,
                    label: tool_labels?.[tool.name] ?? 'destructive',
                  })) ?? [],
            })),
        );
      case '/environments':
        return list([fixture.environment]);
      case `/environments/${fixture.environment.id}`:
        return json(fixture.environment);
      case '/environment-registries':
        return list(['registry.example.test']);
      case '/budgets':
        return json({ items: fixture.budgetRows, time_zone: fixture.policy.time_zone });
      case '/providers':
        return list([fixture.provider]);
      case '/profiles':
        return list([fixture.profile]);
      case '/policy':
        return json(fixture.policy);
      case '/audit':
        return json(fixture.audit);
      case '/proposals':
        return list(fixture.proposals);
      case '/groups':
        return list([
          {
            group_id: fixture.group,
            name: 'Primary',
            concurrency: 12,
            sandbox: { backend: 'kubernetes' },
            busy_ms: 7020,
            idle_ms: 2980,
            idle_stops: 4,
          },
        ]);
      case '/usage':
        return list([
          { kind: 'model', resource_id: fixture.agent.id, cost_usd: '17.50' },
          { kind: 'compute', resource_id: fixture.agent.id, cost_usd: '11.04' },
        ]);
      case `/projects/${fixture.project.id}/agents`:
        return list([fixture.agent]);
      case `/projects/${fixture.project.id}/agents/${fixture.agent.id}`:
        return json(fixture.agent);
      default:
        return route.fulfill({
          json: { error: { message: 'Unsupported mock runner route' } },
          status: 404,
        });
    }
  });
  return { selected, credential: () => credential };
}
type ConnectionFromFixture = (typeof fixture.existingConnections)[number];

test('walks Runner and completes signed OAuth, pinning and project sharing', async ({
  page,
}, info) => {
  const mock = await mockRunner(page);
  const theme = info.project.name.endsWith('-dark') ? 'dark' : 'light';
  await page.addInitScript((preference) => {
    localStorage.setItem('rb.theme', preference);
    localStorage.setItem('runbook.session.kind.v1', 'cookie');
  }, theme);
  const shot = async (name: string) => {
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    await expect
      .poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth))
      .toBeLessThanOrEqual(0);
    await page.evaluate(() => window.scrollTo({ top: 0, behavior: 'instant' }));
    await page.screenshot({
      path: `${shots}/${info.project.name}/${name}.png`,
      fullPage: true,
      animations: 'disabled',
    });
  };

  await page.goto('/settings/organization/runner');
  await expect(page).toHaveURL(/\/runner\/overview$/);
  await expect(page.getByText('$28.54')).toBeVisible();
  await expect(page.getByText('2 queued')).toBeVisible();
  await shot('overview');

  await page.goto('/runner/connections');
  await expect(page.getByRole('heading', { name: 'Warehouse catalog' })).toBeVisible();
  await expect(page.getByText('No sign-in needed')).toHaveCount(2);
  await shot('connections');
  await page.goto('/runner/connections/warehouse?tab=tools');
  await expect(page.getByRole('table', { name: 'Pinned tools' })).toBeVisible();
  await page.getByText('Changed', { exact: true }).click();
  await expect(page.getByText('Input schema changed')).toBeVisible();
  await shot('connection-drift');
  await page.goto('/runner/connections');
  await page.getByRole('link', { name: 'Connect a system' }).click();
  await expect(page.getByRole('heading', { name: 'Choose a system' })).toBeVisible();
  await shot('connect-preset');
  await page.getByRole('button', { name: 'Continue', exact: true }).click();
  await page.getByRole('button', { name: 'Continue to discovery' }).click();
  await page.getByRole('button', { name: 'Discover OAuth endpoints' }).click();
  await expect(page.getByRole('button', { name: 'Confirm endpoints' })).toBeVisible();
  await page.reload();
  await expect(page.getByRole('button', { name: 'Confirm endpoints' })).toBeVisible();
  await shot('connect-endpoints');
  await page.getByRole('button', { name: 'Confirm endpoints' }).click();
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Authorize workspace access' })).toBeVisible();
  await page.getByRole('link', { name: 'Allow access' }).click();
  await expect(page).toHaveURL(/\/runner\/connections\/notion-connection/);
  await expect(page.getByText(/Provider account Northstar Operations/)).toBeVisible();
  await page.getByRole('button', { name: 'Confirm account and connect' }).click();
  await page.getByRole('button', { name: 'Review tools' }).click();
  await page.getByRole('button', { name: 'Discover tools' }).click();
  await expect(page.getByRole('heading', { name: 'Changes since pin' })).toBeVisible();
  await page.reload();
  await page.getByRole('button', { name: 'Review discovered tools' }).click();
  await page.getByLabel('Classification for search').selectOption('read');
  await page.getByRole('checkbox', { name: 'Allow search', exact: true }).check();
  await page.getByRole('button', { name: 'Block all writes' }).click();
  await shot('connect-review-tools');
  await page.getByRole('button', { name: 'Review and pin' }).click();
  await expect(page.getByRole('tab', { name: 'Sharing' })).toHaveAttribute('data-state', 'active');
  await page.getByLabel('Project access').selectOption('selected');
  await page.getByRole('checkbox', { name: 'Operations' }).check();
  await shot('connect-sharing');
  await page.getByRole('button', { name: 'Save project access' }).click();
  await expect(page.getByText('Project access saved.')).toBeVisible();
  expect(mock.selected()).toMatchObject({
    sharing: { projects: [fixture.project.id] },
    pinned_snapshot_id: 'notion_pin',
    never_allowed: ['update_page'],
    tool_labels: { search: 'read' },
  });
  expect(mock.credential()).toMatchObject({ sharing: { projects: [fixture.project.id] } });

  const pages = [
    ['models', '/runner/models', 'Model providers'],
    ['environments', '/runner/environments', 'Allowed image registries'],
    ['environment', '/runner/environments/environment_sample', 'Environment budgets'],
    ['budgets', '/runner/budgets', 'Budgets'],
    ['policy', '/runner/policy', 'Organization budgets'],
    ['agents', '/runner/agents', 'Agents'],
    [
      'agent-editor',
      '/p/operations/agents/agent_sample?tab=configuration',
      'Connections and tools',
    ],
    ['audit', '/runner/audit', 'Audit'],
    ['proposals', '/runner/proposals', 'Change runner catalog'],
    ['credentials', '/runner/credentials', 'Credentials'],
  ];
  for (const [name, url, heading] of pages) {
    await page.goto(url!);
    await expect(
      page.getByRole('heading', { name: heading!, exact: name !== 'proposals' }),
    ).toBeVisible();
    if (name === 'agent-editor') {
      await expect(page.getByRole('checkbox', { name: 'search Read', exact: true })).toBeChecked();
    }
    if (name === 'credentials') {
      await page.getByRole('button', { name: fixture.credentialRow.name }).click();
      await expect(page.getByLabel('Secret source')).toHaveValue('gcp-secret-manager');
    }
    await shot(name!);
  }
});
