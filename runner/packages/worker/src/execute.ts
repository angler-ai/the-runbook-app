import { setLabels } from '@runbook/runner-shared/telemetry';
import { isDeepStrictEqual } from 'node:util';
import { geminiThinking } from '@runbook/runner-shared/gemini-settings';
import { claudeSettings, type ClaudeEffort } from '@runbook/runner-shared/claude-settings';
import type { Redaction } from '@runbook/runner-shared/redact';
import { sandboxIdForClaim, type CapabilityVerifier } from '@runbook/runner-shared/capability';
import type { Config } from '@runbook/runner-shared/config';
import type { Delivery } from '@runbook/runner-shared/contracts/core';
import type { CreateSandboxResponse, ExecResponse, SandboxSize } from '@runbook/runner-shared/contracts/internal';
import type { components } from '@runbook/runner-shared/core-client';
import { gatewayModel } from '@runbook/runner-shared/gateway-model-provider';
import type { ExecutionContext, Outcome } from './claim.ts';
import { clipUtf8, transcriptEvidence } from './evidence.ts';
import { transcriptWriter } from './transcript.ts';
import { InternalError, type workerClients } from './clients.ts';
import { deadline, OAuthBlocked, StepStopped } from './loop/limits.ts';
import { failed, runAgent } from './loop/loop.ts';
import { runNativeAgent } from './loop/native.ts';
import { workspaceToolsV2 } from './loop/workspace-v2.ts';
import { decisionPrompt, nativeStepPrompt, stepPrompt, type RunOrientation } from './loop/prompt.ts';
import { mcpTools, runbookTools, workspaceTools, type AgentTool } from './loop/tools.ts';

interface RunnerStep {
  profile: string;
  provider?: string;
  model?: string;
  effort?: ClaudeEffort;
  task_budget?: boolean;
  mid_conversation_system?: boolean;
  max_output_tokens: number;
  credentials: { credential_id: string; delivery: Delivery }[];
  instructions: string;
  skill_pins?: components['schemas']['RunnerSkillPin'][];
  limits: { max_tokens: number; timeout_s: number; max_cost_usd?: number; max_turns?: number; idle_timeout_s?: number;
    tool_timeout_s?: number; stream_stall_s?: number };
  size: SandboxSize;
  has_mcp: boolean;
  environment?: { image: string | null; has_setup: boolean };
}

export function claimExecutor(config: Config, clients: ReturnType<typeof workerClients>,
  verifier: Pick<CapabilityVerifier, 'verifyCapability'>, image: string,
  observability: { redaction: Redaction; installId: string }) {
  const harness = process.env.RUNNER_HARNESS ?? 'v2';
  if (harness !== 'v1' && harness !== 'v2') throw new Error('RUNNER_HARNESS must be v1 or v2');
  return async (ctx: ExecutionContext): Promise<Outcome> => {
    let mcp: Awaited<ReturnType<typeof mcpTools>> | undefined;
    let wall: ReturnType<typeof deadline> | undefined;
    const started = performance.now();
    try {
      const record = await clients.step<components['schemas']['ClaimedStepRead'] | components['schemas']['ClaimedDecisionRead']>(ctx.stepRunId, ctx.signal);
      const decision = 'kind' in record ? record.decision : undefined;
      const step = 'kind' in record ? undefined : record;
      const claims = await verifier.verifyCapability(ctx.capability());
      if (claims.sub !== ctx.stepRunId || claims.claim_id !== ctx.claim.id || claims.generation !== ctx.claim.generation ||
          claims.runner_id !== config.runnerId || claims.group_id !== config.groupId ||
          Boolean(decision) !== (claims.reach === 'read') || Boolean(decision) !== (ctx.kind === 'decision')) {
        throw new StepStopped('capability_mismatch');
      }
      setLabels({ project: claims.project_id });
      const profile = decision?.model_profile ?? step?.runner_limits?.model;
      const runner = await clients.request<RunnerStep>('api', '/internal/worker/step', {
        body: { capability: ctx.capability(), ...(profile && { profile }) },
        signal: ctx.signal,
      });
      const maxTokens = Math.min(runner.limits.max_tokens, decision?.max_tokens ?? step?.runner_limits?.max_tokens ?? Infinity);
      const timeoutMs = Math.min(runner.limits.timeout_s * 1000, step?.runner_limits?.timeout_ms ?? Infinity);
      const deadlineAt = Math.min(started + timeoutMs,
        performance.now() + ((claims.step_deadline_ms ?? Infinity) - Date.now()));
      const remaining = () => Math.max(1, Math.floor(deadlineAt - performance.now()));
      if (performance.now() >= deadlineAt) return failed('timeout');
      wall = deadline(remaining(), 'timeout');
      const signal = AbortSignal.any([ctx.signal, wall.signal]);

      const rejected = (releaseId: string, tokenVersion: number, signal: AbortSignal) =>
        clients.request('vault', '/internal/oauth/rejected', { signal, timeoutMs: 20_000,
          body: { capability: ctx.capability(), release_id: releaseId, token_version: tokenVersion } });
      for (const release of runner.credentials) {
        signal.throwIfAborted();
        await clients.request('vault', '/internal/release', { body: { capability: ctx.capability(), ...release }, signal, timeoutMs: 25_000 });
      }
      const id = sandboxIdForClaim(claims);
      ctx.registerSandbox(id);
      const sandbox = await clients.request<CreateSandboxResponse>('sandbox-ctl', '/internal/sandboxes', {
        body: { capability: ctx.capability(), size: runner.size, image: runner.environment?.image ?? image }, signal,
        timeoutMs: process.env.RUNNER_SANDBOX_BACKEND === 'kubernetes' ? 320_000 : runner.environment ? 155_000 : 30_000,
      });
      if (sandbox.sandbox_id !== id) return failed('sandbox_identity_mismatch');
      ctx.sandboxStarted();

      const transcript = transcriptWriter(async (role, value) => {
        await clients.request('gateway', '/internal/claims/transcript', {
          body: { capability: ctx.capability(), step_run_id: ctx.stepRunId, role, value }, signal,
        });
      }, observability.redaction, async (value, role) => {
        const response = await clients.request<{ value: unknown }>('gateway', '/internal/claims/redact', {
          body: { capability: ctx.capability(), step_run_id: ctx.stepRunId, role, value }, signal,
        }).catch(() => { throw new StepStopped('redaction_failed'); });
        if (!Object.hasOwn(response, 'value')) throw new StepStopped('redaction_failed');
        return response.value;
      }, signal);
      if (runner.environment?.has_setup) {
        try {
          const result = await clients.request<ExecResponse>('sandbox-ctl', `/internal/sandboxes/${id}/setup`, {
            body: {}, capability: ctx.capability(), signal, timeoutMs: 605_000,
          });
          await transcript.record('tool', { tool: 'environment_setup', result: {
            exit_code: result.exit_code, timed_out: Boolean(result.timed_out),
            stdout: clipUtf8(result.stdout, 8_000), stderr: clipUtf8(result.stderr, 8_000),
          } });
          if (result.exit_code !== 0 || result.timed_out) return failed('environment_setup_failed');
        } catch {
          ctx.signal.throwIfAborted();
          return failed('environment_setup_failed');
        }
      }
      const toolMs = (runner.limits.tool_timeout_s ?? 60) * 1000;
      if (decision) {
        const tools = runbookTools(clients, ctx).filter(({ name }) => name !== 'propose_memory');
        if (runner.has_mcp) {
          const reserved = new Set(['decide_approval', 'report_blocked', 'submit_result', ...tools.map(({ name }) => name)]);
          mcp = await mcpTools(config.urls.gateway!, config.outboundTokens.gateway!, ctx, signal, reserved, true, rejected);
          tools.push(...mcp.tools);
        }
        const orientation = await clients.context<RunOrientation>(ctx.stepRunId, ctx.claim, 'run', signal);
        if (orientation.id) setLabels({ run: orientation.id });
        const prompt = decisionPrompt(decision, runner.instructions, orientation, runner.skill_pins);
        const system = prompt.systemBlocks.join('\n\n');
        await transcript.record('system', system, { clipPrompt: true });
        const input = await transcript.record('input', prompt.input, { clipPrompt: true });
        if (typeof input !== 'string') return failed('redaction_failed');
        const settings = runner.model && claudeSettings(runner.provider ?? '', runner.model, runner.profile, runner.effort);
        const result = await runNativeAgent({
          model: gatewayModel(runner.profile, {
            gatewayUrl: config.urls.gateway!, processToken: config.outboundTokens.gateway!,
            capability: ctx.capability, maxOutputTokens: runner.max_output_tokens, streaming: true,
          }),
          system, systemBlocks: prompt.systemBlocks, input, outputSchema: null,
          decisionOptions: decision.options.map(({ id }) => id), tools, signal,
          limits: { maxTurns: runner.limits.max_turns ?? 60, maxTokens, maxOutputTokens: runner.max_output_tokens,
            timeoutMs: remaining(), idleMs: (runner.limits.idle_timeout_s ?? 300) * 1000,
            toolMs, stallMs: (runner.limits.stream_stall_s ?? 60) * 1000 },
          anthropic: runner.provider === 'anthropic' || runner.provider === 'vertex', taskBudget: runner.task_budget,
          gemini: runner.provider === 'vertex-gemini' ? geminiThinking(runner.profile, runner.effort) : undefined,
          modelOptions: settings ? settings.modelOptions : undefined, midConversationSystem: runner.mid_conversation_system,
          sanitizeTool: (value) => transcript.sanitize(value, 'tool'),
          record: (role, value) => transcript.record(role, value, { sanitized: role === 'tool' &&
            typeof value === 'object' && value !== null && Object.hasOwn(value, 'result') }),
        });
        const finish = result.decision ?? { reason: result.summary || 'Could not complete review.' };
        const safe = await transcript.record('output', finish) as typeof finish | null;
        if ('option' in finish) {
          if (!safe || !('option' in safe) || safe.option !== finish.option ||
              !isDeepStrictEqual(safe.evidence, finish.evidence) || typeof safe.comment !== 'string') {
            return failed('redaction_failed');
          }
          return { ...result, decision: { ...safe, comment: clipUtf8(safe.comment, 4096) } };
        }
        const reason = safe && 'reason' in safe && typeof safe.reason === 'string' ? clipUtf8(safe.reason, 4096) : 'Could not complete review.';
        return { ...result, decision: { reason } };
      }
      if (!step) return failed('runner_step_unavailable');
      const workspace = harness === 'v2' ? workspaceToolsV2(clients, ctx, id, toolMs, (value) => transcript.sanitize(value, 'tool')) : undefined;
      const progress = async (text: string) => {
        try {
          const safe = await transcript.sanitize(text, 'tool');
          if (typeof safe === 'string') {
            await clients.logs(ctx.stepRunId, ctx.claim, safe.split('\n').map((line) => clipUtf8(line, 1000)), signal);
          }
        } catch {
          // Progress is best effort. An unavailable redactor must never send raw text.
        }
      };
      const checklistUpdate: AgentTool = {
        name: 'update_checklist', description: 'Mark a checklist item done using the [call N] references of supporting tool calls.',
        schema: { type: 'object', properties: { title: { type: 'string' }, tool_call_ids: { type: 'array',
          items: { type: 'string' }, minItems: 1 } }, required: ['title', 'tool_call_ids'], additionalProperties: false },
        async call(args, toolSignal) {
          const title = String(args.title);
          if (!step.checklist.includes(title)) return { error: 'Unknown checklist title.' };
          await clients.checklist(ctx.stepRunId, ctx.claim, [title], toolSignal);
          await progress(`Checklist completed: ${title}. Evidence: ${(args.tool_call_ids as string[]).join(', ')}.`);
          return { ok: true, title };
        },
      };
      const tools = [
        ...(workspace ? [...workspace.tools, checklistUpdate] :
          workspaceTools(clients, ctx, id, toolMs, (content) => transcript.sanitize(content, 'tool'))),
        ...runbookTools(clients, ctx),
      ];
      if (runner.has_mcp) {
        const builtIns = new Set(['submit_result', 'report_blocked', ...tools.map(({ name }) => name)]);
        mcp = await mcpTools(config.urls.gateway!, config.outboundTokens.gateway!, ctx, signal, workspace && builtIns, false, rejected);
        tools.push(...(workspace ? mcp.tools : mcp.tools.filter(({ name }) => !builtIns.has(name))));
      }
      const orientation = await clients.context<RunOrientation>(ctx.stepRunId, ctx.claim, 'run', signal);
      if (orientation.id) setLabels({ run: orientation.id });
      const model = gatewayModel(runner.profile, {
        gatewayUrl: config.urls.gateway!, processToken: config.outboundTokens.gateway!,
        capability: ctx.capability, maxOutputTokens: runner.max_output_tokens, streaming: Boolean(workspace),
      });
      const common = {
        model, outputSchema: step.output_schema, checklist: step.checklist, check: step.runner_limits?.check ?? false, tools, signal,
        limits: { maxTurns: runner.limits.max_turns ?? 60, maxTokens, maxOutputTokens: runner.max_output_tokens,
          timeoutMs: remaining(), idleMs: (runner.limits.idle_timeout_s ?? 300) * 1000,
          toolMs, stallMs: (runner.limits.stream_stall_s ?? (workspace ? 60 : 45)) * 1000 },
      };
      let result;
      if (workspace) {
        const prompt = nativeStepPrompt(step, runner.instructions, orientation, runner.skill_pins);
        const system = prompt.systemBlocks.join('\n\n');
        await transcript.record('system', system, { clipPrompt: true });
        const safe = await transcript.record('input', prompt.input, { clipPrompt: true });
        if (typeof safe !== 'string') return failed('redaction_failed');
        let input = safe;
        if (Buffer.byteLength(input) > 24 * 1024) {
          const boundary = safe.lastIndexOf('\n');
          const path = await workspace.storeInput(safe.slice(boundary + 1), signal);
          const instructions = safe.slice(0, boundary).replace(/\n\nStep data:$/, '');
          input = `${clipUtf8(instructions, 8000)}\n\nRead the complete step data, including inputs and any previous attempt, at ${path}.`;
        }
        const settings = runner.model && claudeSettings(runner.provider ?? '', runner.model, runner.profile, runner.effort);
        result = await runNativeAgent({ ...common, system, systemBlocks: prompt.systemBlocks, input, maxCostUsd: runner.limits.max_cost_usd,
          anthropic: runner.provider === 'anthropic' || runner.provider === 'vertex', taskBudget: runner.task_budget,
          gemini: runner.provider === 'vertex-gemini' ? geminiThinking(runner.profile, runner.effort) : undefined,
          modelOptions: settings ? settings.modelOptions : undefined,
          midConversationSystem: runner.mid_conversation_system, sanitizeTool: (value) => transcript.sanitize(value, 'tool'),
          record: (role, value) => transcript.record(role, value, { sanitized: role === 'tool' &&
            typeof value === 'object' && value !== null && Object.hasOwn(value, 'result') }),
          progress, storeResult: workspace.storeResult });
        if (result.zeroToolCalls) await progress('Result submitted without any working tool calls.');
        if (result.blockKind) await progress(`Reported blocked: ${result.blockKind}.`);
      } else {
        const prompt = stepPrompt(step, runner.instructions, orientation, runner.skill_pins);
        const system = await transcript.record('system', prompt.system);
        const input = await transcript.record('input', prompt.input);
        if (typeof system !== 'string' || typeof input !== 'string') return failed('redaction_failed');
        result = await runAgent({ ...common, input, system, record: transcript.record });
      }
      const { completed, ...outcome } = result;
      delete (outcome as { zeroToolCalls?: boolean }).zeroToolCalls;
      delete (outcome as { blockKind?: string }).blockKind;
      const safeOutput = await transcript.record('output', { summary: outcome.summary ?? '', outputs: outcome.outputs });
      const safe = safeOutput as Pick<Outcome, 'summary' | 'outputs'> | null;
      if (typeof safe?.summary !== 'string' || !safe.outputs || typeof safe.outputs !== 'object' || Array.isArray(safe.outputs)) {
        return failed('redaction_failed');
      }
      if (step.output_schema != null && !isDeepStrictEqual(outcome.outputs, safe.outputs)) return failed('output_redacted');
      const evidence = transcriptEvidence(config.coreUrl!, observability.installId, ctx.stepRunId, transcript.excerpt(), step.evidence);
      const required = step.evidence?.required ?? [];
      if (outcome.status === 'succeeded' && required.some((kind) =>
        kind === 'log' ? !evidence.logs.length : kind === 'link' ? !evidence.evidence.length : true)) {
        return { ...failed('step_evidence_unavailable'), ...evidence };
      }
      if (completed?.length) await clients.checklist(ctx.stepRunId, ctx.claim, completed, signal);
      return { ...outcome, outputs: safe.outputs, summary: clipUtf8(safe.summary, 4096), ...evidence };
    } catch (error) {
      ctx.signal.throwIfAborted();
      if (wall?.signal.aborted) return failed((wall.signal.reason as StepStopped).reason);
      if (error instanceof OAuthBlocked || (error instanceof InternalError && error.code === 'oauth.reconnect_required')) {
        const reason = error instanceof OAuthBlocked ? error.reason : new OAuthBlocked('missing_access').reason;
        return { ...failed(reason), status: 'blocked', ...(ctx.kind === 'decision' && { decision: { reason } }) };
      }
      if (error instanceof StepStopped) return failed(error.reason);
      if (error instanceof InternalError && error.code === 'sandbox.image_not_approved') return failed('environment_image_not_approved');
      if (error instanceof InternalError && error.status === 402) return failed(error.budgetDetail ?? `limit:${error.level ?? 'unknown'}`);
      return failed('runner_step_unavailable');
    } finally {
      wall?.close();
      await mcp?.close().catch(() => undefined);
    }
  };
}
