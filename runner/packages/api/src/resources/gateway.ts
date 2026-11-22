import type pg from 'pg';
import type { CapabilityVerifier } from '@runbook/runner-shared/capability';
import type { Delivery, GrantClaims, Label } from '@runbook/runner-shared/contracts/core';
import { MAX_TRANSCRIPT_BYTES, TRANSCRIPT_ROLES, type TranscriptRole, type EgressPolicy, type ToolCallRecord } from '@runbook/runner-shared/contracts/internal';
import { claudeSettings, type ClaudeEffort } from '@runbook/runner-shared/claude-settings';
import { modelCachePricing } from '@runbook/runner-shared/model-pricing';
import { HttpError, type Route } from '@runbook/runner-shared/http';
import { sharedWith, validateEgress, type AgentConfig } from './agents.ts';

interface ProfileRow {
  id: string;
  name: string;
  model: string;
  options: { max_output_tokens?: unknown; max_response_bytes?: number; effort?: ClaudeEffort };
  sharing: unknown;
  provider_sharing: unknown;
  type: string;
  endpoint: string;
  verification: string;
  credential_id: string;
  models: { id?: unknown; input_usd_per_million?: unknown; output_usd_per_million?: unknown; cached_usd_per_million?: number; cache_write_usd_per_million?: number }[];
}

function denied(): never {
  throw new HttpError(403, 'gateway.resource_unavailable', 'gateway resource unavailable');
}

export function gatewayResources(pool: pg.Pool, verifier: Pick<CapabilityVerifier, 'verifyCapability'>, orgId = ''): Route[] {
  async function pinned(body: unknown) {
    const capability = (body as { capability?: unknown } | null)?.capability;
    if (typeof capability !== 'string') denied();
    const claims = await verifier.verifyCapability(capability);
    const { rows: [version] } = await pool.query<{ config: AgentConfig }>(
      `SELECT v.config FROM agent_versions v JOIN agents a ON a.id = v.agent_id
       WHERE v.id = $1 AND a.id = $2 AND a.project_id = $3 AND a.group_id = $4 AND a.status = 'active'`,
      [claims.agent_version, claims.agent_id, claims.project_id, claims.group_id],
    );
    if (!version) denied();
    return { claims, config: version.config };
  }

  async function labels(claims: GrantClaims, config: AgentConfig, delivery: Delivery, modelCredential?: string) {
    const allowed = modelCredential ? [modelCredential] : config.credentials
      .filter((item) => item.delivery === delivery &&
        (claims.connections === null || claims.connections.includes(item.credential_id)))
      .map(({ credential_id }) => credential_id);
    const bound = claims.reach === 'read' ? allowed.filter((id) => claims.connections?.includes(id)) : allowed;
    const { rows } = await pool.query<{ id: string; label: Label }>(
      `SELECT id, label FROM credentials WHERE id = ANY($1::text[]) AND org_id = $2 AND status = 'active'
       AND (sharing->>'projects' = 'all' OR (sharing->'projects') ? $3)
       AND delivery ? $4 AND (groups = '"all"'::jsonb OR groups ? $5)`,
      [bound, orgId, claims.project_id, delivery, claims.group_id],
    );
    return Object.fromEntries(rows.filter(({ label }) => claims.reach !== 'read' || label === 'read').map(({ id, label }) => [id, label])) as Record<string, Label>;
  }

  function allowsCredential(labels: Record<string, Label>, id: string, destructive: boolean) {
    if (!Object.hasOwn(labels, id)) return false;
    return destructive || labels[id] === 'read' || labels[id] === 'write';
  }

  async function modelProfile(claims: GrantClaims, config: AgentConfig, profile: string | undefined) {
    if (!Array.isArray(config.profiles) ||
        (claims.step_deadline_ms != null && claims.step_deadline_ms <= Date.now()) ||
        (claims.model_profile != null && profile !== undefined && profile !== claims.model_profile)) denied();
    const { rows: [row] } = await pool.query<ProfileRow>(
      `SELECT p.id, p.name, p.model, p.options, p.sharing, v.sharing AS provider_sharing, v.type, v.endpoint,
         v.credential_id, v.models, concat(p.updated_at::text, '|', v.updated_at::text) AS verification
       FROM profiles p JOIN providers v ON v.id = p.provider_id
       WHERE ($1::text IS NULL OR p.name = $1) AND p.id = ANY($2::text[])
       ORDER BY array_position($2::text[], p.id) LIMIT 1`, [claims.model_profile ?? profile ?? null, config.profiles],
    );
    if (!row || (claims.reach === 'read' && claims.model_id !== row.model) || !sharedWith(row.sharing, claims.project_id) || !sharedWith(row.provider_sharing, claims.project_id) ||
        !['anthropic', 'openai-compatible', 'vertex', 'vertex-gemini', 'bedrock'].includes(row.type) || !row.endpoint ||
        !allowsCredential(await labels(claims, config, 'model', row.credential_id), row.credential_id, claims.destructive)) denied();
    const model = Array.isArray(row.models) ? row.models.find((item) => item.id === row.model) : undefined;
    const limit = row.options.max_output_tokens ?? config.limits?.max_tokens;
    if (!model || typeof model.input_usd_per_million !== 'number' ||
        typeof model.output_usd_per_million !== 'number' ||
        !Number.isFinite(model.input_usd_per_million) || model.input_usd_per_million < 0 ||
        !Number.isFinite(model.output_usd_per_million) || model.output_usd_per_million < 0 ||
        typeof limit !== 'number' || !Number.isSafeInteger(limit) || limit <= 0 ||
        !Number.isSafeInteger(config.limits?.max_tokens) || config.limits.max_tokens <= 0) denied();
    return { id: row.id, verification: row.verification, name: row.name, effort: row.options.effort, credentialId: row.credential_id, provider: row.type, model: row.model, endpoint: row.endpoint,
      inputUsdPerMillion: model.input_usd_per_million, outputUsdPerMillion: model.output_usd_per_million,
      ...modelCachePricing(row.type, row.model, model.input_usd_per_million, model.cached_usd_per_million, model.cache_write_usd_per_million),
      servedPrices: row.models.filter((item) => typeof item.id === 'string' &&
        typeof item.input_usd_per_million === 'number' && typeof item.output_usd_per_million === 'number').map((item) => ({
        model: item.id, inputUsdPerMillion: item.input_usd_per_million, outputUsdPerMillion: item.output_usd_per_million,
        ...modelCachePricing(row.type, String(item.id), Number(item.input_usd_per_million),
          item.cached_usd_per_million, item.cache_write_usd_per_million),
      })),
      ...(Number.isSafeInteger(row.options.max_response_bytes) && row.options.max_response_bytes! > 0 &&
        { maxResponseBytes: row.options.max_response_bytes }),
      maxOutputTokens: Math.min(limit, config.limits.max_tokens, claims.max_tokens ?? Infinity) };
  }

  return [{
    method: 'POST', path: '/internal/model/profile', callers: ['gateway'],
    async handle({ body }) {
      const { claims, config } = await pinned(body);
      const profile = (body as { profile?: unknown })?.profile;
      if (typeof profile !== 'string') denied();
      return { body: await modelProfile(claims, config, profile) };
    },
  }, {
    method: 'POST', path: '/internal/model/verified', callers: ['gateway'],
    async handle({ body }) {
      const { claims, config } = await pinned(body);
      const { profile, verification } = body as { profile?: unknown; verification?: unknown };
      if (typeof profile !== 'string' || typeof verification !== 'string') denied();
      const selected = await modelProfile(claims, config, profile);
      const changed = await pool.query(
        `UPDATE profiles p SET verified_at = now() FROM providers v
         WHERE p.id = $1 AND v.id = p.provider_id
           AND concat(p.updated_at::text, '|', v.updated_at::text) = $2`,
        [selected.id, verification],
      );
      if (!changed.rowCount) throw new HttpError(409, 'model.profile_changed', 'model profile changed during verification');
      return { status: 204 };
    },
  }, {
    method: 'POST', path: '/internal/worker/step', callers: ['worker'],
    async handle({ body, runnerId }) {
      const { claims, config } = await pinned(body);
      const profile = (body as { profile?: unknown })?.profile;
      if (claims.runner_id !== runnerId || (profile !== undefined && typeof profile !== 'string')) denied();
      const selected = await modelProfile(claims, config, profile);
      const { rows: [group] } = await pool.query<{ sandbox: { size?: string } }>(
        'SELECT sandbox FROM groups WHERE group_id = $1', [claims.group_id],
      );
      if (!group) denied();
      const credentials: { credential_id: string; delivery: Delivery }[] = [
        { credential_id: selected.credentialId, delivery: 'model' },
      ];
      for (const delivery of ['proxy', 'mcp'] as const) {
        const allowed = await labels(claims, config, delivery);
        for (const id of Object.keys(allowed)) {
          if (allowsCredential(allowed, id, claims.destructive)) credentials.push({ credential_id: id, delivery });
        }
      }
      const settings = claudeSettings(selected.provider, selected.model, selected.name, selected.effort);
      return { body: {
        profile: selected.name, provider: selected.provider, model: selected.model, effort: selected.effort,
        ...(settings.anthropic && { task_budget: settings.taskBudget, mid_conversation_system: settings.midConversationSystem }),
        max_output_tokens: selected.maxOutputTokens, credentials,
        instructions: config.instructions ?? '', skill_pins: config.skill_pins ?? [], limits: { ...config.limits,
          timeout_s: Math.min(config.limits.timeout_s, (claims.timeout_ms ?? Infinity) / 1000),
          max_tokens: Math.min(config.limits.max_tokens, claims.max_tokens ?? Infinity),
        }, has_mcp: Boolean(config.mcp?.length),
        size: config.sandbox?.size ?? group.sandbox.size ?? 'small',
      } };
    },
  }, {
    method: 'POST', path: '/internal/egress/policy', callers: ['gateway'],
    async handle({ body }) {
      const { claims, config } = await pinned(body);
      const { rows: [policy] } = await pool.query<{ default_egress: NonNullable<AgentConfig['egress']> }>(
        'SELECT default_egress FROM policy',
      );
      if (!policy) denied();
      const hosts = validateEgress(config.egress);
      const narrowing = claims.network?.map((host) => host.toLowerCase());
      const rules = (entries: typeof hosts) => Object.fromEntries(entries
        .filter(({ label }) => claims.reach !== 'read' || label === 'read')
        .map(({ host, label }) => [host, label])) as EgressPolicy['layers'][number];
      const reply: EgressPolicy = {
        destructive: claims.destructive,
        credentialLabels: await labels(claims, config, 'proxy'),
        layers: [rules(validateEgress(policy.default_egress)), rules(hosts),
          rules(hosts.filter(({ host }) => !narrowing || narrowing.includes(host)))],
      };
      return { body: reply };
    },
  }, {
    method: 'POST', path: '/internal/mcp/snapshot', callers: ['gateway'],
    async handle({ body }) {
      const { claims, config } = await pinned(body);
      const destructive = claims.destructive;
      const { rows } = await pool.query<{
        id: string; url: string; transport: string; credential_id: string | null; sharing: unknown;
        tool_labels: Record<string, Label>; never_allowed: string[]; tools: { name: string }[];
      }>(
        `SELECT m.id, m.url, m.transport, m.credential_id, m.sharing, m.tool_labels, m.never_allowed, s.tools
         FROM mcp_servers m JOIN mcp_tool_snapshots s ON s.server_id = m.id AND s.id = m.pinned_snapshot_id
         WHERE m.id = ANY($1::text[])`,
        [(config.mcp ?? []).map(({ server_id }) => server_id)],
      );
      const credentialLabels = await labels(claims, config, 'mcp');
      const servers = rows.flatMap((row) => {
        if (!sharedWith(row.sharing, claims.project_id) || (row.credential_id &&
            !allowsCredential(credentialLabels, row.credential_id, destructive))) return [];
        const tools = row.tools.flatMap((tool) => {
          const label = Object.hasOwn(row.tool_labels, tool.name) ? row.tool_labels[tool.name] : 'destructive';
          if ((claims.reach === 'read' && label !== 'read') || (!destructive && label !== 'read' && label !== 'write') || row.never_allowed.includes(tool.name) ||
              !config.mcp?.some((entry) => entry.server_id === row.id && entry.tool === tool.name)) return [];
          return [{ tool, label }];
        });
        return tools.length ? [{ id: row.id, url: row.url, transport: row.transport,
          ...(row.credential_id && { credential_id: row.credential_id }), tools }] : [];
      });
      return { body: { destructive, credentialLabels, servers } };
    },
  }, {
    method: 'POST', path: '/internal/transcripts', callers: ['gateway'], maxBodyBytes: MAX_TRANSCRIPT_BYTES + 8192,
    async handle({ body }) {
      const record = body as { capability?: unknown; role?: TranscriptRole; value?: unknown } | null;
      if (typeof record?.capability !== 'string' || !record.role || !TRANSCRIPT_ROLES.includes(record.role) ||
          !Object.hasOwn(record, 'value')) {
        throw new HttpError(400, 'transcript.invalid_record', 'invalid transcript record');
      }
      const entry = JSON.stringify({ role: record.role, value: record.value });
      if (Buffer.byteLength(entry) > MAX_TRANSCRIPT_BYTES) {
        throw new HttpError(413, 'transcript.chunk_too_large', 'transcript chunk is too large');
      }
      const claims = await verifier.verifyCapability(record.capability);
      await pool.query(
        `INSERT INTO transcripts(step_run_id, group_id, runner_id, project_id, agent_id, claim_id, generation, entry)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)`,
        [claims.sub, claims.group_id, claims.runner_id, claims.project_id, claims.agent_id, claims.claim_id, claims.generation, entry],
      );
      return { status: 204 };
    },
  }, {
    method: 'POST', path: '/internal/mcp/tool-calls', callers: ['gateway'],
    async handle({ body }) {
      const row = body as Partial<ToolCallRecord> | undefined;
      const size = (value: unknown) => typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 2_147_483_647;
      if (!row || typeof row.capability !== 'string' || typeof row.server_id !== 'string' || !row.server_id ||
          typeof row.tool !== 'string' || !row.tool || !row.args_redacted || typeof row.args_redacted !== 'object' ||
          Array.isArray(row.args_redacted) || !size(row.args_bytes) || !size(row.result_bytes) || !size(row.duration_ms) ||
          (row.error !== undefined && row.error !== 'tool_failed' && row.error !== 'tool_unavailable')) {
        throw new HttpError(400, 'mcp.invalid_record', 'invalid tool call record');
      }
      const claims = await verifier.verifyCapability(row.capability);
      await pool.query(
        `INSERT INTO tool_calls(step_run_id, group_id, runner_id, project_id, agent_id, server_id, tool,
           args_redacted, args_bytes, result_bytes, duration_ms, error) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)`,
        [claims.sub, claims.group_id, claims.runner_id, claims.project_id, claims.agent_id,
          row.server_id, row.tool, JSON.stringify(row.args_redacted), row.args_bytes, row.result_bytes, row.duration_ms, row.error ?? null],
      );
      return { status: 204 };
    },
  }];
}
