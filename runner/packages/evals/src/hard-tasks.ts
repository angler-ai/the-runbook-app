import { data, suite } from './task-fixtures.ts';
import type { Task } from './tasks.ts';

const tool = (description: string, value: unknown, args: Record<string, string>) => ({
  ...data(description, value), arguments: args,
});

export const hardTasks: Task[] = [
  ...suite('investigation', ['settlement', 'dispatch', 'replenishment'], 1, (name, i) => {
    const shard = 4 + i;
    const target = `job-${713 + i}`;
    const mcp = Object.fromEntries(Array.from({ length: 6 }, (_, index) => {
      const n = index + 1;
      const limit = 60 + n * 3;
      return [`shard_${n}`, data(`Read live budget and lag counter for shard ${n}`,
        { budgetSeconds: limit, actualSeconds: limit + (n === shard ? 17 + i : -n), generation: 4 })];
    }));
    return {
      files: {
        'scope.txt': `incident=${name}\ngeneration=4\nshards=1-6\n`,
        'jobs.csv': 'generation,shard,job\n' + Array.from({ length: 6 }, (_, index) =>
          `3,${index + 1},stale-${index}\n4,${index + 1},${index + 1 === shard ? target : 'good-' + index}\n`).join(''),
        'handoff.log': `job=job-700 trace=prior stage=done\njob=${target} trace=tr-${50 + i} stage=handoff\njob=${target} trace=old-${i} stage=queued\n`,
        'workers.log': `trace=prior owner=sync\ntrace=old-${i} owner=loader\ntrace=tr-${50 + i} owner=team-${name}\n`,
      },
      mcp,
      instructions: 'Use scope.txt and check every shard with its live MCP counter. A shard is bad only if actualSeconds exceeds budgetSeconds for the scoped generation. Join the bad shard to jobs.csv by generation and shard, then follow that job’s handoff-stage trace in handoff.log to its owner in workers.log. Return shard (number), overSeconds (number), job, trace and owner (strings).',
      expected: { outputs: { shard, overSeconds: 17 + i, job: target, trace: `tr-${50 + i}`, owner: `team-${name}` },
        minCalls: 7, requiredTools: Object.keys(mcp) },
    };
  }, 'expert'),

  ...suite('causal-trace', ['double-write', 'late-ack', 'silent-drop'], 1, (name, i) => {
    const trace = `r${900 + i}`;
    const rule = ['key_epoch', 'ack_partition', 'codec_version'][i]!;
    return {
      files: {
        'brief.txt': `Customer symptom: ${name}. Operations suspects quota.`,
        'edge.log': `10:00 ERROR quota trace=quick status=429\n10:01 WARN trace=${trace} status=202\n10:05 ERROR trace=${trace} status=timeout\n`,
        'queue.log': `trace=quick outcome=rejected\ntrace=${trace} path=old fallback=enabled\ntrace=${trace} path=new handoff=evt-${i}\n`,
        'consumer.log': `evt=evt-${i} attempt=1 ${rule}=4 work=provisional\nevt=evt-${i} attempt=2 ${rule}=3 work=discarded\n`,
      },
      mcp: {
        routing: data('Get current delivery attempt and minimum accepted field value', { trace, authoritativePath: 'new', authoritativeAttempt: 2, field: rule, minimum: 4 }),
        quota: data('Get the trace affected by the active quota alert', { trace: 'quick', remaining: 0 }),
      },
      instructions: 'Identify the timeout cause. Trace the timed-out request from edge.log through the authoritative path and attempt in routing, queue.log and consumer.log. Check the quota alert separately. Return trace, event (the handoff ID), field (the field whose accepted minimum was violated), shortfall (minimum minus observed as a number), and excludedTrace (the trace on the quota alert).',
      expected: { outputs: { trace, event: `evt-${i}`, field: rule, shortfall: 1, excludedTrace: 'quick' },
        minCalls: 5, requiredTools: ['routing', 'quota'] },
    };
  }, 'expert'),

  ...suite('partial-access', ['payouts', 'credits', 'charges'], 1, (name, i) => ({
    files: { 'scope.txt': `Include only rows for ${name} in posted state; the verifiedRegion is west.\n`,
      'west.csv': `stream,state,cents\n${name},posted,${1800 + i}\n${name},pending,9000\nother,posted,3020\n${name},posted,45\n` },
    mcp: {
      permissions: data('Read the account’s region-to-scope mapping', { east: `${name}:east:read`, west: 'local snapshot' }),
      east_ledger: data('Read east finalized ledger', { error: '403 missing_access', scope: `${name}:east:read` }),
    },
    instructions: 'Verify the west subtotal in integer cents from the scoped local view. Inspect permissions and attempt east_ledger. If east denies access, block with kind missing_access. Use the block reason (or summary for a JSON answer) as one JSON object with exactly verifiedRegion (string), verifiedCents (number), and deniedScope (string). Do not submit a global total.',
    expected: { kind: 'missing_access', minCalls: 3, requiredTools: ['permissions', 'east_ledger'],
      blockedSummary: { verifiedRegion: 'west', verifiedCents: 1845 + i, deniedScope: `${name}:east:read` } },
  }), 'hard'),

  ...suite('tested-edit', ['webhook', 'settler', 'archiver'], 1, (name, i) => {
    const value = 20 + i * 7;
    const script = "set -eu\ngrep -Fx \"prod_limit=$1\" service.conf\ngrep -Fx \"| prod | $1 |\" operations.md\nprintf 'verified\\n'\n";
    const command = `bash verify.sh ${value}`;
    const config = 'dev_limit=7\nprod_limit=7\n';
    const docs = '| stage | limit |\n| dev | 7 |\n| prod | 7 |\n';
    return {
      files: { 'request.txt': `component=${name}\nprod_limit=${value}\n`, 'service.conf': config,
        'operations.md': docs, 'verify.sh': script },
      instructions: 'Read request.txt. Update only the prod limit value in service.conf and operations.md, preserving every other byte. Run bash verify.sh with the requested limit as its sole argument after both edits, and require its success. Return component (string from request.txt) and limit (number).',
      expected: { outputs: { component: name, limit: value }, minCalls: 2, requiredTools: ['verified_command'],
        verification: { command, stdout: `prod_limit=${value}\n| prod | ${value} |\nverified\n` },
        edits: { 'service.conf': config.replace('prod_limit=7', `prod_limit=${value}`),
          'operations.md': docs.replace('| prod | 7 |', `| prod | ${value} |`) } },
    };
  }, 'hard'),

  ...suite('background', ['aggregation', 'packing'], 0, (name, i) => ({
    files: { 'job.sh': `#!/bin/sh\nset -eu\nprintf 'started\n'\nsleep 2\nprintf 'job=${name} rows=%s\n' "$((${13 + i} * 17 - 9))"\n` },
    instructions: 'Start bash job.sh in the background. Use the background job ID to poll its output until it emits its job record and completes successfully. Return job (string after job=) and rows (number after rows=).',
    expected: { outputs: { job: name, rows: (13 + i) * 17 - 9 }, minCalls: 2, requiredTools: ['bash_background', 'completed_background'] },
  }), 'hard'),

  ...suite('filtered-log', ['manifest', 'stream', 'segment'], 1, (name, i) => {
    const wanted = `op-${77 + i}`;
    return {
      files: {
        'cursor.txt': `operation=${wanted}\n`,
        'bulk.log': `op=${wanted} stage=plan disposition=discarded digest=dead\n` +
          'INFO unrelated op=op-00 stage=commit disposition=committed digest=0000\n'.repeat(4900) +
          `op=${wanted} stage=commit disposition=committed digest=a0${42 + i}\n` +
          'INFO unrelated op=op-01 stage=commit disposition=committed digest=1111\n'.repeat(5100) +
          `op=${wanted} stage=commit disposition=rolled_back digest=beef\n`,
      },
      instructions: `Read cursor.txt and use grep or ripgrep (including a native grep tool) to filter bulk.log before reading records. Find the scoped ${name} operation’s committed commit record; ignore plan and rollback rows. Return operation and digest (strings as recorded).`,
      expected: { outputs: { operation: wanted, digest: `a0${42 + i}` }, minCalls: 2, requiredTools: ['grep'] },
    };
  }, 'hard'),

  ...suite('guarded-edit', ['backfill', 'migration'], 0, (name, i) => {
    const generation = 40 + i;
    const decision = i === 1 ? 'applied' : 'skipped';
    const live = decision === 'applied' ? generation : generation + 1;
    return {
      files: { 'request.txt': `job=${name}\nexpected=${generation}\n`, 'flags.conf': 'staging=off\nprod=off\n' },
      mcp: { generation: { ...data('Read current generation and in-flight status', { generation: live, inFlight: false }),
        arguments: {}, unchanged: { 'flags.conf': 'staging=off\nprod=off\n' } } },
      instructions: 'Check generation through MCP before editing. Enable prod in flags.conf (change prod=off to prod=on only) if and only if its generation equals expected in request.txt and inFlight is false. Otherwise make no changes. Return decision (applied or skipped) and liveGeneration (number).',
      expected: { outputs: { decision, liveGeneration: live }, minCalls: 2, requiredTools: ['generation'],
        ...(decision === 'applied' && { edits: { 'flags.conf': 'staging=off\nprod=on\n' } }) },
    };
  }, 'hard'),

  ...suite('memory-conflict', ['warehouse', 'notifier'], 0, (name, i) => ({
    files: { 'change.txt': `service=${name}\nminimumRevision=${40 + i}\n`,
      'snapshot.csv': `revision,leader\n${39 + i},legacy\n${40 + i},replica-${i}\n` },
    memory: `Previous incident notes: ${name} leader was legacy at revision ${39 + i}. Use as historical context only.`,
    mcp: { deployment: data('Read the latest deployment’s minimum committed revision', { revision: 40 + i, state: 'committed' }) },
    instructions: 'Use change.txt, deployment, and the current snapshot to identify the leader at the committed revision. Treat remembered assignments as historical. Return leader (string), revision (number) and superseded (the former leader string).',
    expected: { outputs: { leader: `replica-${i}`, revision: 40 + i, superseded: 'legacy' }, minCalls: 3, requiredTools: ['deployment'] },
  }), 'hard'),

  ...suite('repair-retry', ['copy', 'publish', 'transfer'], 0, (name, i) => {
    const first = `obj-${200 + i}`;
    const second = `obj-${300 + i}`;
    const ledger = `${first},committed\n`;
    return {
      previousAttempt: 'The process died after writing one commit. Its cursor remained at the start of the batch.',
      files: { 'ledger.csv': ledger, 'cursor.txt': 'next=0\n', 'batch.csv': `${first},commit\n${second},commit\nignored,skip\n` },
      mcp: { journal: data('Read acknowledged commits for this batch', { acknowledged: [first], batch: name }) },
      instructions: 'Recover the whole batch. Cross-check journal with ledger.csv before changing files. Append exactly the uncommitted commit row as ID,committed plus newline. Do not duplicate an acknowledged row or add a skip row. Update cursor.txt to next=3 followed by newline. Return resumedAt (the existing committed ID), appended (the new committed ID) and next (number).',
      expected: { outputs: { resumedAt: first, appended: second, next: 3 }, minCalls: 4, requiredTools: ['journal'],
        edits: { 'ledger.csv': ledger + `${second},committed\n`, 'cursor.txt': 'next=3\n' } },
    };
  }, 'expert'),

  ...suite('runbook-context', ['watermark', 'throughput', 'lag'], 1, (name, i) => {
    const earlier = `step-${30 + i}`;
    const past = `run-${70 + i}`;
    return {
      files: { 'scope.txt': `Metric: ${name}. Compare with the latest succeeded baseline run for this metric.\n` },
      mcp: {
        run_overview: data('Read this run’s steps and inputs', { steps: [
          { id: 'superseded', name: 'Extract', status: 'failed' }, { id: earlier, name: 'Extract', status: 'succeeded' },
        ] }),
        step_details: tool('Read a completed step by step_run_id', { outputs: { [name]: 89 + i } }, { step_run_id: earlier }),
        search_runs: tool('Search prior run names or tags by q', { runs: [
          { id: 'failed', status: 'failed' }, { id: past, status: 'succeeded' }, { id: 'older', status: 'succeeded' },
        ] }, { q: name }),
        get_run: tool('Read an earlier run by run_id', { outputs: { [name]: 78 - i } }, { run_id: past }),
      },
      instructions: `Use Runbook tools run_overview, step_details, search_runs (q = "${name}", newest first), and get_run to compare this run’s successful Extract output with the newest succeeded baseline. Return stepId and baselineId (strings) and increase (current minus baseline as a number).`,
      expected: { outputs: { stepId: earlier, baselineId: past, increase: 11 + 2 * i }, minCalls: 4,
        requiredTools: ['run_overview', 'step_details', 'search_runs', 'get_run'] },
    };
  }, 'expert'),

  ...suite('precision', ['storage', 'egress', 'snapshots'], 0, (name, i) => ({
    files: { 'events.csv': 'timestamp,state,bytes\n' +
      `2026-11-01T01:20:00-07:00,settled,${1610612736 + i * 13107200}\n` +
      '2026-11-01T01:20:00-08:00,settled,536870912\n' +
      '2026-11-01T01:30:00-08:00,settled,1073741824\n' +
      '2026-11-01T01:15:00-07:00,void,1073741824\n' +
      '2026-11-01T00:59:59-07:00,settled,1073741824\n' },
    mcp: { window: data('Read the billing window and rate', { startUtc: '2026-11-01T08:00:00Z',
      endUtc: '2026-11-01T09:30:00Z', centsPerGiB: 37.25 + i }) },
    instructions: `Compute ${name} cost for settled rows in the MCP time window, inclusive start and exclusive end. Apply each row’s UTC offset, including the repeated local hour. Sum bytes before converting to GiB (2^30 bytes), multiply by centsPerGiB, and round the final total to the nearest integer cent with ties upward. Return rows and cents (numbers) and latestUtc (latest included event as YYYY-MM-DDTHH:mm:ssZ).`,
    expected: { outputs: { rows: 2, cents: [75, 77, 79][i]!, latestUtc: '2026-11-01T09:20:00Z' },
      minCalls: 2, requiredTools: ['window'] },
  }), 'expert'),
];
