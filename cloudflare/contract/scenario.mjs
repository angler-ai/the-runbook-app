// CF9: the scripted requests the contract suite sends to each build. One
// pass covers every public route of CF0's manifest (run.mjs checks), in
// the order a project lives: discovery, directory, variables, tokens,
// authoring and skills, memory, webhooks, a run driven through every
// step-run route (signal, agent claim and logs, evidence upload, human
// checklist, approval), its report, rerun and execution tokens, and the
// error answers of routes this edition does not serve (device login with
// sign-in off, archives before the daily job).
//
// Each `t.call` is recorded with its label; both builds must produce the
// same list. Reads of rows Cloudflare derives within a bounded delay
// (D56: run lists, the inbox, deliveries) wait with `t.until` until they
// show what the scenario just did, so only the settled answer is recorded.

const b64 = (text) => Buffer.from(text).toString('base64');

function runbookSource(owner) {
  return [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: contract, title: Contract run, description: The contract suite run, tags: [contract, cf9]}',
    'spec:',
    '  inputs: {service: {type: string, default: api}}',
    '  steps:',
    '    - {id: gate, wait: {signal: go, timeout: 1h}}',
    '    - id: check',
    '      title: Check the service',
    '      actor: agent',
    "      skill: 'skill://contract-check'",
    '      outputs: {code: {type: integer, required: true}}',
    '    - id: tick',
    '      title: Tick the list',
    '      actor: human',
    `      assignee: "user:${owner}"`,
    '      checklist: [First, Second]',
    '    - id: decide',
    '      approval: {prompt: Keep it?, options: [keep, drop], approvers: {teams: [oncall]}}',
    '',
  ].join('\n');
}

const CALL_CHILD = [
  'apiVersion: therunbook.app/v1',
  'kind: Runbook',
  'metadata: {id: contract-child, title: Contract child}',
  'spec:',
  '  inputs: {target: {type: string, required: true}}',
  '  steps:',
  '    - {id: migrate, title: Migrate, actor: agent, outputs: {rows: {type: integer, required: true}}}',
  '  outputs:',
  "    rows: '${{ steps.migrate.outputs.rows }}'",
  '',
].join('\n');
const CALL_PARENT = [
  'apiVersion: therunbook.app/v1',
  'kind: Runbook',
  'metadata: {id: contract-parent, title: Contract parent}',
  'spec:',
  '  steps:',
  '    - id: db',
  '      call: {runbook: contract-child, with: {target: prod}, outputs: {rows: {type: integer}}}',
  "    - {id: after, title: 'After ${{ steps.db.outputs.rows }} rows', actor: agent}",
  '',
].join('\n');

/** LANG4: publish a child and a parent that calls it; run the parent to
 * the end through its child, then cancel a second parent while its child
 * runs. */
async function callScenario(t, project) {
  const publish = async (label, source) => {
    const created = await t.call(`${label} create`, 'POST', '/v1/projects/{p}/runbooks', { p: project }, {
      body: { source },
    });
    const rb = created.body.runbook_id;
    const draft = await t.call(`${label} draft`, 'GET', '/v1/runbooks/{rb}/draft', { rb });
    await t.call(`${label} publish`, 'POST', '/v1/runbooks/{rb}/versions', { rb }, {
      ifMatch: draft.headers.get('etag'),
      body: {},
    });
  };
  // Before the child is published the call cannot resolve.
  await t.call('call validate unresolved', 'POST', '/v1/validate', {}, {
    body: { source: CALL_PARENT, project: 'default' },
  });
  await publish('call child', CALL_CHILD);
  await t.call('call validate', 'POST', '/v1/validate', {}, { body: { source: CALL_PARENT, project: 'default' } });
  await publish('call parent', CALL_PARENT);

  const childOf = (r) => (r.body?.steps ?? []).find((s) => s.path === 'db')?.call;
  const runParent = async (label) => {
    const started = await t.call(`${label} start`, 'POST', '/v1/projects/{p}/executions', { p: project }, {
      body: { runbook: 'contract-parent' },
      key: label,
    });
    const parent = started.body.id;
    const linked = await t.until(`${label} parent waiting`, 'GET', '/v1/executions/{exe}', { exe: parent }, (r) =>
      childOf(r)?.started === true,
    );
    const child = childOf(linked).child_execution_id;
    const running = await t.until(`${label} child`, 'GET', '/v1/executions/{exe}', { exe: child }, (r) =>
      (r.body?.steps ?? []).some((s) => s.path === 'migrate' && s.status === 'ready'),
    );
    return { parent, child, running };
  };

  const { parent, child, running } = await runParent('call');
  const migrate = (running.body?.steps ?? []).find((s) => s.path === 'migrate')?.id;
  const claimed = await t.call('call child claim', 'POST', '/v1/step-runs/{sr}/claim', { sr: migrate }, {
    token: t.service,
    body: { session_id: 'contract-call', actor: { kind: 'agent', agent_name: 'contract-agent' } },
  });
  const claim = { id: claimed.body.claim?.id ?? claimed.body.id, generation: claimed.body.claim?.generation ?? claimed.body.generation };
  await t.call('call child report', 'POST', '/v1/step-runs/{sr}/report', { sr: migrate }, {
    token: t.service,
    body: { step_run_id: migrate, claim, status: 'succeeded', outputs: { rows: 7 } },
  });
  await t.until('call child done', 'GET', '/v1/executions/{exe}', { exe: child }, (r) => r.body?.status === 'succeeded');
  await t.until('call parent after', 'GET', '/v1/executions/{exe}', { exe: parent }, (r) =>
    (r.body?.steps ?? []).some((s) => s.path === 'after'),
  );
  await t.call('call child report markdown', 'GET', '/v1/executions/{exe}/report', { exe: child }, {
    query: { format: 'markdown' },
  });

  // Cancelling the parent cancels its running child.
  const second = await runParent('call cancel');
  await t.call('call cancel parent', 'PATCH', '/v1/executions/{exe}', { exe: second.parent }, {
    body: { action: 'cancel' },
  });
  await t.until('call cancel child', 'GET', '/v1/executions/{exe}', { exe: second.child }, (r) =>
    r.body?.status === 'cancelled',
  );
  await t.until('call cancel parent done', 'GET', '/v1/executions/{exe}', { exe: second.parent }, (r) =>
    r.body?.status === 'cancelled',
  );
}

// LANG7: a lean runbook (no apiVersion, kind or titles) with an agent task
// a person takes over and two strict assignees, one that resolves to
// nobody (a routing error) and one naming the oncall team.
const LANG7 = [
  'metadata: {id: contract-lang7}',
  'spec:',
  "  inputs: {owner: {type: string, default: 'user:nobody-here'}}",
  '  steps:',
  '    - id: all',
  '      parallel:',
  '        - {id: build, actor: agent}',
  '        - {id: page, actor: human, assignee: team:oncall, assignment: strict}',
  "        - {id: stuck, actor: human, assignee: '${{ inputs.owner }}', assignment: strict}",
  '        - {id: anyone, actor: any, assignee: team:oncall, assignment: strict}',
  '',
].join('\n');

// LANG9b: a webhook trigger: a start, a duplicate (the same `dedupe`
// value), a call `when` skips, an invalid payload and an unknown trigger.
const TRIGGERS = [
  'metadata: {id: contract-triggers}',
  'spec:',
  '  inputs: {service: {type: string, required: true}}',
  '  triggers:',
  '    webhook:',
  '      - id: on_deploy',
  '        payload: {status: {type: string, required: true}, service: {type: string}, id: {type: integer}}',
  "        when: \"${{ payload.status == 'success' }}\"",
  '        inputs: {service: "${{ payload.service }}"}',
  '        dedupe: "${{ payload.id }}"',
  '  steps:',
  '    - {id: work, title: Work}',
  '',
].join('\n');

async function triggerScenario(t, project) {
  const created = await t.call('trigger create', 'POST', '/v1/projects/{p}/runbooks', { p: project }, {
    body: { source: TRIGGERS },
  });
  const rb = created.body.runbook_id;
  const draft = await t.call('trigger draft', 'GET', '/v1/runbooks/{rb}/draft', { rb });
  await t.call('trigger publish', 'POST', '/v1/runbooks/{rb}/versions', { rb }, {
    ifMatch: draft.headers.get('etag'),
    body: {},
  });
  const path = '/v1/projects/{p}/runbooks/{rb}/triggers/{trigger}';
  const params = { p: project, rb: 'contract-triggers', trigger: 'on_deploy' };
  const deploy = { status: 'success', service: 'api', id: 1, extra: { ignored: true } };
  await t.call('trigger start', 'POST', path, params, { body: deploy });
  await t.call('trigger duplicate', 'POST', path, params, { body: deploy });
  await t.call('trigger skipped', 'POST', path, params, { body: { status: 'failure', id: 2 } });
  await t.call('trigger invalid payload', 'POST', path, params, { body: { service: 5 } });
  await t.call('trigger unknown', 'POST', path, { ...params, trigger: 'nope' }, { body: {} });
}

// LANG7 review round 1: a strict task must name its assignee.
const LANG7_NO_ASSIGNEE =
  'metadata: {id: contract-lang7-bare}\nspec:\n  steps:\n    - {id: page, assignment: strict}\n';

/** LANG7: strict assignment and take-over, through the claim route. */
async function lang7Scenario(t, project) {
  await t.call('lang7 validate', 'POST', '/v1/validate', {}, { body: { source: LANG7, project: 'default' } });
  await t.call('lang7 validate strict without assignee', 'POST', '/v1/validate', {}, {
    body: { source: LANG7_NO_ASSIGNEE, project: 'default' },
  });
  const created = await t.call('lang7 create', 'POST', '/v1/projects/{p}/runbooks', { p: project }, {
    body: { source: LANG7 },
  });
  const rb = created.body.runbook_id;
  const draft = await t.call('lang7 draft', 'GET', '/v1/runbooks/{rb}/draft', { rb });
  await t.call('lang7 publish', 'POST', '/v1/runbooks/{rb}/versions', { rb }, {
    ifMatch: draft.headers.get('etag'),
    body: {},
  });
  const started = await t.call('lang7 start', 'POST', '/v1/projects/{p}/executions', { p: project }, {
    body: { runbook: 'contract-lang7' },
    key: 'lang7',
  });
  const exe = started.body.id;
  // Ready, and the unresolved strict step's blocked notice recorded (it
  // reaches no subscriber; Cloudflare derives that row within D56's delay).
  const ready = await t.until('lang7 ready', 'GET', '/v1/executions/{exe}', { exe }, (r) =>
    ['build', 'page', 'stuck', 'anyone'].every((path) =>
      (r.body?.steps ?? []).some((s) => s.path === path && s.status === 'ready'),
    ) && (r.body?.notifications_not_delivered ?? []).some((n) => n.event_type === 'step.blocked'),
  );
  const step = (path) => (ready.body?.steps ?? []).find((s) => s.path === path)?.id;
  const [build, page, stuck, anyone] = [step('build'), step('page'), step('stuck'), step('anyone')];
  const person = (label, sr, extra = {}) =>
    t.call(label, 'POST', '/v1/step-runs/{sr}/claim', { sr }, {
      body: { session_id: `contract-${label}`, actor: { kind: 'human' }, ...extra },
      key: label,
    });
  const agent = { session_id: 'contract-lang7-agent', actor: { kind: 'agent', agent_name: 'contract-agent' } };
  // Review round 1: a strict `actor: any` task is its assignee's (a
  // person's): agents are neither offered it nor allowed to claim it.
  await t.call('lang7 agent brief', 'GET', '/v1/executions/{exe}/next', { exe }, { token: t.service });
  await t.call('lang7 agent claim strict any', 'POST', '/v1/step-runs/{sr}/claim', { sr: anyone }, {
    token: t.service,
    body: agent,
    key: 'lang7-agent-strict-any',
  });
  // A person claims an agent task only by taking it over, never a live claim.
  await person('lang7 claim agent task', build);
  await t.call('lang7 agent claim', 'POST', '/v1/step-runs/{sr}/claim', { sr: build }, { token: t.service, body: agent });
  await person('lang7 take over live claim', build, { take_over: true });
  await t.call('lang7 admin release', 'POST', '/v1/step-runs/{sr}/release', { sr: build }, {
    body: { reason: 'the agent is stuck' },
  });
  await t.call('lang7 agent take over', 'POST', '/v1/step-runs/{sr}/claim', { sr: build }, {
    token: t.service,
    body: { ...agent, take_over: true },
    key: 'lang7-agent-take-over',
  });
  await person('lang7 take over', build, { take_over: true });
  // Strict assignees: nobody eligible, then the oncall team.
  await person('lang7 claim unresolved strict', stuck);
  await person('lang7 claim strict', page);
  // The unresolved strict step's blocked notice reaches no subscriber; on
  // Cloudflare that row is derived within a bounded delay (D56).
  await t.until('lang7 document', 'GET', '/v1/executions/{exe}/report', { exe }, (r) =>
    (r.body?.header?.notifications_not_delivered ?? []).some((n) => n.event_type === 'step.blocked'),
  );
  await t.call('lang7 report markdown', 'GET', '/v1/executions/{exe}/report', { exe }, {
    query: { format: 'markdown' },
  });
  await t.call('lang7 cancel', 'PATCH', '/v1/executions/{exe}', { exe }, { body: { action: 'cancel' } });
}

// UI-3: a run whose view shows the graph overlay's facts: the branch a
// switch took, a step skipped by its `if`, and a foreach's iteration count.
const OVERLAY = [
  'apiVersion: therunbook.app/v1',
  'kind: Runbook',
  'metadata: {id: contract-overlay, title: Contract overlay}',
  'spec:',
  '  inputs: {regions: {type: list, items: {type: string}, default: [a, b]}}',
  '  vars: {picked: none}',
  '  steps:',
  '    - id: gate',
  '      switch:',
  '        cases:',
  "          - when: '${{ false }}'",
  '            steps: [{id: never, title: Never, actor: agent}]',
  '        default:',
  '          steps: [{id: chosen, set: {vars: {picked: default}}}]',
  "    - {id: maybe, title: Maybe, actor: agent, if: '${{ false }}'}",
  '    - id: each',
  "      foreach: {items: '${{ inputs.regions }}', as: region, max_items: 4, steps: [{id: work, title: Work, actor: agent}]}",
  '',
].join('\n');

async function overlayScenario(t, project) {
  const created = await t.call('overlay create', 'POST', '/v1/projects/{p}/runbooks', { p: project }, {
    body: { source: OVERLAY },
  });
  const rb = created.body.runbook_id;
  const draft = await t.call('overlay draft', 'GET', '/v1/runbooks/{rb}/draft', { rb });
  await t.call('overlay publish', 'POST', '/v1/runbooks/{rb}/versions', { rb }, {
    ifMatch: draft.headers.get('etag'),
    body: {},
  });
  const started = await t.call('overlay start', 'POST', '/v1/projects/{p}/executions', { p: project }, {
    body: { runbook: 'contract-overlay' },
    key: 'overlay',
  });
  const exe = started.body.id;
  const step = (r, path) => (r.body?.steps ?? []).find((s) => s.path === path);
  await t.until('overlay view', 'GET', '/v1/executions/{exe}', { exe }, (r) =>
    step(r, 'gate')?.branch?.kind === 'default' &&
    step(r, 'maybe')?.skip_reason === 'condition' &&
    step(r, 'each')?.iterations?.total === 2 &&
    (step(r, 'each')?.iterations?.labels ?? []).join(',') === 'a,b' &&
    step(r, 'each')?.iterations?.started >= 1,
  );
  await t.call('overlay document', 'GET', '/v1/executions/{exe}/report', { exe });
  await t.call('overlay cancel', 'PATCH', '/v1/executions/{exe}', { exe }, { body: { action: 'cancel' } });
  await t.until('overlay cancelled', 'GET', '/v1/executions/{exe}', { exe }, (r) => r.body?.status === 'cancelled');
}

export async function scenario(t) {
  const { org, project, user } = t;
  // The agent step is claimed with the project's service token (an agent).
  // ---- Discovery and the caller.
  await t.call('discovery', 'GET', '/.well-known/runbook', {}, { token: null });
  await t.call('meta', 'GET', '/v1/meta', {}, { token: null });
  await t.call('me', 'GET', '/v1/me');
  await t.call('me anonymous', 'GET', '/v1/me', {}, { token: null });
  // MCP2: connected OAuth apps. Sign-in is off on both builds, so the owner
  // has no auth identity: no apps, and disconnecting one is 404.
  await t.call('connected apps', 'GET', '/v1/me/apps');
  await t.call('disconnect an unknown app', 'DELETE', '/v1/me/apps/{client}', { client: 'noSuchClient' });
  await t.call('orgs', 'GET', '/v1/orgs');

  // ---- MCP2: OAuth discovery for /mcp. Sign-in is off on both builds, so
  // there is no authorization server: its documents are unknown routes, and
  // /mcp's 401 names no metadata.
  for (const path of [
    '/.well-known/oauth-protected-resource',
    '/.well-known/oauth-protected-resource/mcp',
    '/.well-known/oauth-authorization-server',
    '/.well-known/oauth-authorization-server/api/auth',
  ]) {
    await t.call(`oauth discovery ${path}`, 'GET', path, {}, { token: null });
  }
  const initialize = { jsonrpc: '2.0', id: 0, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'contract', version: '1' } } };
  await t.call('mcp without a bearer', 'POST', '/mcp', {}, { token: null, body: initialize, recordHeaders: ['www-authenticate'] });
  await t.call('mcp with an unknown oauth token', 'POST', '/mcp', {}, {
    token: 'trb_oat_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    body: initialize,
    recordHeaders: ['www-authenticate'],
  });
  await t.call('mcp with the pat', 'POST', '/mcp', {}, { body: initialize, recordHeaders: ['www-authenticate'] });

  // ---- Directory.
  const ops = await t.call('project create', 'POST', '/v1/orgs/{org}/projects', { org }, {
    body: { slug: 'ops', title: 'Operations' },
  });
  await t.call('project duplicate', 'POST', '/v1/orgs/{org}/projects', { org }, {
    body: { slug: 'ops', title: 'Operations' },
    key: 'project-again',
  });
  await t.call('projects', 'GET', '/v1/orgs/{org}/projects', { org });
  const alice = await t.call('user create', 'POST', '/v1/orgs/{org}/users', { org }, { body: { name: 'Alice' } });
  const bob = await t.call('user create 2', 'POST', '/v1/orgs/{org}/users', { org }, { body: { name: 'Bob' } });
  await t.call('users', 'GET', '/v1/orgs/{org}/users', { org });
  await t.call('user deactivate', 'PATCH', '/v1/orgs/{org}/users/{user}', { org, user: bob.body.id }, {
    body: { active: false },
  });
  await t.call('user reactivate', 'PATCH', '/v1/orgs/{org}/users/{user}', { org, user: bob.body.id }, {
    body: { active: true },
  });
  const team = await t.call('team create', 'POST', '/v1/orgs/{org}/teams', { org }, {
    body: { slug: 'oncall', title: 'On call' },
  });
  await t.call('teams', 'GET', '/v1/orgs/{org}/teams', { org });
  for (const [label, member] of [['owner', user], ['alice', alice.body.id]]) {
    await t.call(`team add ${label}`, 'POST', '/v1/orgs/{org}/teams/{team}/members', { org, team: team.body.id }, {
      body: { user_id: member },
    });
  }
  await t.call('team members', 'GET', '/v1/orgs/{org}/teams/{team}/members', { org, team: team.body.id });
  await t.call('team remove alice', 'DELETE', '/v1/orgs/{org}/teams/{team}/members/{user}', {
    org,
    team: team.body.id,
    user: alice.body.id,
  });
  await t.call('project member add', 'POST', '/v1/projects/{p}/members', { p: project }, {
    body: { user_id: alice.body.id, role: 'operator' },
  });
  await t.call('project member add 2', 'POST', '/v1/projects/{p}/members', { p: project }, {
    body: { user_id: bob.body.id, role: 'viewer' },
  });
  await t.call('project members', 'GET', '/v1/projects/{p}/members', { p: project });
  await t.call('project member remove', 'DELETE', '/v1/projects/{p}/members/{user}', { p: project, user: bob.body.id });
  const invitation = await t.call('invite', 'POST', '/v1/orgs/{org}/invitations', { org }, {
    body: { email: 'invitee@example.test', role: 'member' },
  });
  await t.call('invitations', 'GET', '/v1/orgs/{org}/invitations', { org });
  await t.call('invite revoke', 'DELETE', '/v1/orgs/{org}/invitations/{inv}', {
    org,
    inv: invitation.body?.id ?? 'inv_00000000000000000000000000',
  });
  await t.call('other project', 'GET', '/v1/projects/{p}/members', { p: ops.body.id });

  // ---- SEARCH2: one search across the caller's projects; another org is
  // unknown to them.
  await t.call('search everything', 'GET', '/v1/orgs/{org}/search', { org }, { query: { q: 'contract' } });
  await t.call('search empty', 'GET', '/v1/orgs/{org}/search', { org });
  await t.call('search other org', 'GET', '/v1/orgs/{org}/search', { org: 'org_00000000000000000000000009' }, {
    query: { q: 'contract' },
  });

  // ---- NOTIFY1 / SLACK1: email preferences, one-click unsubscribe and a
  // project's Slack URL (set and cleared at once, so nothing is posted).
  await t.call('email preferences', 'GET', '/v1/me/email-preferences');
  await t.call('email preferences set', 'PUT', '/v1/me/email-preferences', {}, {
    body: { preference: 'approvals_only' },
  });
  await t.call('email preferences invalid', 'PUT', '/v1/me/email-preferences', {}, {
    body: { preference: 'loud' },
  });
  await t.call('unsubscribe unknown token', 'POST', '/v1/unsubscribe', {}, {
    token: null,
    body: { token: 'usr_00000000000000000000000000.00000000000000000000000000000000' },
  });
  await t.call('slack', 'GET', '/v1/projects/{p}/slack', { p: project });
  await t.call('slack invalid', 'PUT', '/v1/projects/{p}/slack', { p: project }, {
    body: { url: 'https://example.com/hook' },
  });
  await t.call('slack set', 'PUT', '/v1/projects/{p}/slack', { p: project }, {
    body: { url: 'https://hooks.slack.com/services/T0000000/B0000000/contract' },
  });
  await t.call('slack clear', 'DELETE', '/v1/projects/{p}/slack', { p: project });
  // Stage 2 without a Slack app: no channel, no interactions endpoint.
  await t.call('slack channel without app', 'PUT', '/v1/projects/{p}/slack', { p: project }, {
    body: { channel: 'C0123ABCDEF' },
  });
  await t.call('slack interactions without app', 'POST', '/v1/slack/interactions', {}, {
    token: null,
    body: { payload: '{}' },
  });

  // ---- Profiles and project settings (2026-11-17): rename yourself, a
  // photo set, read and removed, a project renamed with an image. Only the
  // image's header is read, so a bare PNG header stands in for one.
  await t.call('me rename', 'PATCH', '/v1/me', {}, { body: { name: 'Contract Owner' } });
  await t.call('me rename invalid', 'PATCH', '/v1/me', {}, { body: { name: ' ' } });
  await t.call('picture none', 'GET', '/v1/users/{user}/picture', { user });
  await t.call('picture set', 'PUT', '/v1/me/picture', {}, { body: { data: 'iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAAAAAAAA' } });
  await t.call('picture', 'GET', '/v1/users/{user}/picture', { user });
  await t.call('picture too large', 'PUT', '/v1/me/picture', {}, { body: { data: 'iVBORw0KGgoAAAANSUhEUgAAAlgAAAAKCAYAAAAAAAAA' } });
  await t.call('picture remove', 'DELETE', '/v1/me/picture');
  await t.call('project rename', 'PATCH', '/v1/projects/{p}', { p: project }, { body: { title: 'Contract project' } });
  await t.call('project picture none', 'GET', '/v1/projects/{p}/picture', { p: project });
  await t.call('project picture set', 'PUT', '/v1/projects/{p}/picture', { p: project }, { body: { data: 'iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAAAAAAAA' } });
  await t.call('project picture', 'GET', '/v1/projects/{p}/picture', { p: project });
  await t.call('project picture remove', 'DELETE', '/v1/projects/{p}/picture', { p: project });
  // Archived projects (2026-11-18): read-only until restored.
  await t.call('project archive', 'PATCH', '/v1/projects/{p}', { p: project }, { body: { archived: true } });
  await t.call('archived project list', 'GET', '/v1/orgs/{org}/projects', { org });
  await t.call('archived project runbooks', 'GET', '/v1/projects/{p}/runbooks', { p: project });
  await t.call('archived project write', 'PUT', '/v1/projects/{p}/slack', { p: project }, {
    body: { url: 'https://hooks.slack.com/services/T0000000/B0000000/archived' },
  });
  await t.call('archived project rename', 'PATCH', '/v1/projects/{p}', { p: project }, { body: { title: 'Nope' } });
  await t.call('project restore', 'PATCH', '/v1/projects/{p}', { p: project }, { body: { archived: false } });

  // ---- Git sync (2026-11-17): a folder published as a batch. A dry run,
  // a refused batch (one bad file), the publish recording its commit, the
  // same files again (unchanged), the draft locked, then a prune.
  const synced = (id) =>
    ['apiVersion: therunbook.app/v1', 'kind: Runbook', `metadata: {id: ${id}, title: Synced}`, 'spec:', '  steps:',
      '    - {id: work, title: Work}', ''].join('\n');
  const syncFiles = [{ path: 'runbooks/contract-synced.yaml', source: synced('contract-synced') }];
  await t.call('sync dry run', 'POST', '/v1/projects/{p}/sync', { p: project }, {
    body: { files: syncFiles, dry_run: true },
  });
  await t.call('sync invalid', 'POST', '/v1/projects/{p}/sync', { p: project }, {
    body: { files: [...syncFiles, { path: 'runbooks/copy.yaml', source: synced('contract-synced') }] },
  });
  const firstSync = await t.call('sync', 'POST', '/v1/projects/{p}/sync', { p: project }, {
    body: { files: syncFiles, commit_sha: 'abc123', branch: 'main' },
  });
  await t.call('sync again', 'POST', '/v1/projects/{p}/sync', { p: project }, {
    body: { files: syncFiles, commit_sha: 'def456' },
  });
  const syncedRb = firstSync.body?.runbooks?.[0]?.runbook_id;
  if (syncedRb) {
    await t.call('sync version', 'GET', '/v1/runbooks/{rb}/versions/{version}', { rb: syncedRb, version: '1' });
    await t.call('sync versions', 'GET', '/v1/runbooks/{rb}/versions', { rb: syncedRb });
    const syncedDraft = await t.call('sync draft', 'GET', '/v1/runbooks/{rb}/draft', { rb: syncedRb });
    await t.call('sync draft locked', 'PATCH', '/v1/runbooks/{rb}/draft', { rb: syncedRb }, {
      ifMatch: syncedDraft.headers.get('etag'),
      body: { source: synced('contract-synced') + '# edited\n' },
    });
  }
  if (syncedRb) {
    await t.call('sync detach', 'POST', '/v1/runbooks/{rb}/detach', { rb: syncedRb });
    await t.call('sync detach again', 'POST', '/v1/runbooks/{rb}/detach', { rb: syncedRb });
    // The next sync adopts it back.
    await t.call('sync after detach', 'POST', '/v1/projects/{p}/sync', { p: project }, {
      body: { files: syncFiles },
    });
  }
  await t.call('sync prune', 'POST', '/v1/projects/{p}/sync', { p: project }, {
    body: { files: [], prune: true },
  });

  // ---- Project variables.
  const vars = await t.call('vars', 'GET', '/v1/projects/{p}/vars', { p: project });
  const declared = await t.call('vars put', 'PUT', '/v1/projects/{p}/vars', { p: project }, {
    ifMatch: vars.headers.get('etag'),
    body: {
      declarations: { region: { type: 'string' }, retries: { type: 'integer' } },
      values: { region: 'us-west', retries: 3 },
    },
  });
  await t.call('vars stale', 'PUT', '/v1/projects/{p}/vars', { p: project }, {
    ifMatch: vars.headers.get('etag'),
    body: { declarations: {}, values: {} },
  });
  const one = await t.call('var', 'GET', '/v1/projects/{p}/vars/{name}', { p: project, name: 'region' });
  const put = await t.call('var put', 'PUT', '/v1/projects/{p}/vars/{name}', { p: project, name: 'region' }, {
    ifMatch: one.headers.get('etag') ?? declared.headers.get('etag'),
    body: { declaration: { type: 'string' }, value: 'eu-west' },
  });
  const after = await t.call('var delete', 'DELETE', '/v1/projects/{p}/vars/{name}', { p: project, name: 'retries' }, {
    ifMatch: put.headers.get('etag'),
  });
  const again = await t.call('vars again', 'GET', '/v1/projects/{p}/vars', { p: project });
  await t.call('vars delete', 'DELETE', '/v1/projects/{p}/vars', { p: project }, {
    ifMatch: again.headers.get('etag') ?? after.headers.get('etag'),
  });

  // ---- Tokens.
  // The bootstrap mints the owner's PAT and the project's service token in
  // one transaction, at one store time: their ID order is their random
  // part's, so this list is compared as a set.
  await t.call('tokens', 'GET', '/v1/tokens', {}, { unordered: true });
  const minted = await t.call('token create', 'POST', '/v1/tokens', {}, {
    body: { name: 'contract', scopes: ['runbooks:read', 'executions:read'], projects: [project] },
  });
  await t.call('token me', 'GET', '/v1/me', {}, { token: minted.body.secret });
  const second = await t.call('token create 2', 'POST', '/v1/tokens', {}, {
    body: { name: 'contract-2', scopes: ['runbooks:read'], projects: [project] },
  });
  await t.call('token revoke', 'DELETE', '/v1/tokens/{id}', { id: second.body.token?.id ?? second.body.id });
  await t.call('token self revoke', 'DELETE', '/v1/me/token', {}, { token: minted.body.secret });
  await t.call('token revoked', 'GET', '/v1/me', {}, { token: minted.body.secret });

  // ---- Webhooks (to a local receiver).
  const hook = await t.call('webhook create', 'POST', '/v1/projects/{p}/webhooks', { p: project }, {
    body: { name: 'Receiver', url: t.receiver, events: ['*'] },
    hide: ['secret'],
  });
  const id = hook.body.webhook?.id ?? hook.body.id;
  await t.call('webhooks', 'GET', '/v1/projects/{p}/webhooks', { p: project });
  await t.call('webhook', 'GET', '/v1/projects/{p}/webhooks/{id}', { p: project, id });
  await t.call('webhook patch', 'PATCH', '/v1/projects/{p}/webhooks/{id}', { p: project, id }, {
    body: { name: 'Receiver (renamed)' },
  });
  await t.call('webhook rotate', 'POST', '/v1/projects/{p}/webhooks/{id}/rotate-secret', { p: project, id }, {
    body: {},
  });
  await t.call('webhook test', 'POST', '/v1/projects/{p}/webhooks/{id}/test', { p: project, id }, { body: {} });

  // ---- Authoring.
  const source = runbookSource(user);
  await t.call('validate', 'POST', '/v1/validate', {}, { body: { source, project: 'default' } });
  await t.call('validate invalid', 'POST', '/v1/validate', {}, { body: { source: 'kind: Nope\n' } });
  const skill = await t.call('skill upload', 'POST', '/v1/projects/{p}/skills', { p: project }, {
    body: {
      files: [
        { path: 'SKILL.md', content: b64('---\nname: contract-check\ndescription: Checks a service.\n---\nRun the check.\n') },
        { path: 'check.sh', content: b64('#!/bin/sh\necho ok\n') },
      ],
      reference: 'contract',
    },
  });
  await t.call('skills', 'GET', '/v1/projects/{p}/skills', { p: project });
  await t.call('skill', 'GET', '/v1/projects/{p}/skills/{hash}', { p: project, hash: skill.body.hash });
  const created = await t.call('runbook create', 'POST', '/v1/projects/{p}/runbooks', { p: project }, {
    body: { source },
  });
  const rb = created.body.runbook_id;
  await t.call('runbooks', 'GET', '/v1/projects/{p}/runbooks', { p: project });
  await t.call('runbook', 'GET', '/v1/runbooks/{rb}', { rb });
  // UI-1c: the same runbook by its slug (the project by ID and by slug), and a slug no runbook has.
  await t.call('runbook by slug', 'GET', '/v1/projects/{p}/runbooks/{slug}', { p: project, slug: 'contract' });
  await t.call('runbook by slug, project slug', 'GET', '/v1/projects/{p}/runbooks/{slug}', { p: 'default', slug: 'contract' });
  await t.call('runbook by slug missing', 'GET', '/v1/projects/{p}/runbooks/{slug}', { p: project, slug: 'contract-missing' });
  let draft = await t.call('draft', 'GET', '/v1/runbooks/{rb}/draft', { rb });
  const v1 = await t.call('publish', 'POST', '/v1/runbooks/{rb}/versions', { rb }, {
    ifMatch: draft.headers.get('etag'),
    body: {},
  });
  draft = await t.call('draft edit', 'PATCH', '/v1/runbooks/{rb}/draft', { rb }, {
    ifMatch: (await t.call('draft again', 'GET', '/v1/runbooks/{rb}/draft', { rb })).headers.get('etag'),
    body: { source: source.replace('title: Contract run', 'title: Contract run v2') },
  });
  await t.call('draft stale', 'PATCH', '/v1/runbooks/{rb}/draft', { rb }, {
    ifMatch: '"0"',
    body: { source },
  });
  const v2 = await t.call('publish 2', 'POST', '/v1/runbooks/{rb}/versions', { rb }, {
    ifMatch: draft.headers.get('etag'),
    body: {},
  });
  await t.call('versions', 'GET', '/v1/runbooks/{rb}/versions', { rb });
  await t.call('version', 'GET', '/v1/runbooks/{rb}/versions/{version}', { rb, version: '1' });
  await t.call('version form', 'GET', '/v1/runbooks/{rb}/versions/{version}/form', { rb, version: '2' });
  await t.call('version diff', 'GET', '/v1/runbooks/{rb}/versions/{a}/diff/{b}', { rb, a: '1', b: '2' });
  void v1;
  void v2;

  // Every delivery so far (the test event and both publications) settled.
  const deliveries = await t.until(
    'webhook deliveries',
    'GET',
    '/v1/projects/{p}/webhooks/{id}/deliveries',
    { p: project, id },
    (r) =>
      (r.body?.items?.length ?? 0) >= 3 &&
      r.body.items.every((d) => d.state === 'delivered'),
  );
  const delivery = deliveries.body?.items?.[0]?.id ?? 'whd_00000000000000000000000000';
  await t.call('webhook redeliver', 'POST', '/v1/projects/{p}/webhooks/{id}/deliveries/{d}/redeliver', {
    p: project,
    id,
    d: delivery,
  }, { body: {} });

  // ---- Memory.
  const mem = await t.call('memory create', 'POST', '/v1/projects/{p}/memory', { p: project }, {
    body: { title: 'Restart order', body_markdown: 'Restart the API before the workers.', tags: ['ops'] },
  });
  await t.call('memory list', 'GET', '/v1/projects/{p}/memory', { p: project });
  const memory = await t.call('memory get', 'GET', '/v1/memory/{mem}', { mem: mem.body.id });
  await t.call('memory archive', 'PATCH', '/v1/memory/{mem}', { mem: mem.body.id }, {
    ifMatch: memory.headers.get('etag'),
    body: { action: 'archive' },
  });
  await t.call('runbook memory create', 'POST', '/v1/runbooks/{rb}/memory', { rb }, {
    body: { title: 'Known flake', body_markdown: 'The first check can flake.', tags: [], step_id: 'check' },
  });
  await t.call('runbook memory list', 'GET', '/v1/runbooks/{rb}/memory', { rb });
  // MEM6: a service token (an agent) saves live memory and edits it with
  // If-Match; it never accepts, rejects or archives, and proposals (asked
  // for) and history stay out of its reads. A person reads the revision
  // history and rejects the proposal.
  const saved = await t.call('memory save (service)', 'POST', '/v1/runbooks/{rb}/memory', { rb }, {
    token: t.service,
    body: { title: 'Agent fact', body_markdown: 'Checks take a minute.', tags: [], step_id: 'check' },
  });
  const sid = saved.body?.id ?? 'mem_00000000000000000000000000';
  await t.call('memory edit (service)', 'PATCH', '/v1/memory/{mem}', { mem: sid }, {
    token: t.service,
    ifMatch: '"r1"',
    body: { action: 'edit', entry: { title: 'Agent fact', body_markdown: 'Checks take two minutes.', tags: [], step_id: 'check' } },
  });
  await t.call('memory archive (service)', 'PATCH', '/v1/memory/{mem}', { mem: sid }, {
    token: t.service,
    ifMatch: '"r2"',
    body: { action: 'archive' },
  });
  await t.call('memory history (service)', 'GET', '/v1/memory/{mem}/revisions', { mem: sid }, { token: t.service });
  await t.call('memory history', 'GET', '/v1/memory/{mem}/revisions', { mem: sid });
  // MEM6 review: a page of history; a person's lock refuses agent edits
  // until unlocked; restore brings back a prior revision, even archived.
  await t.call('memory history page', 'GET', '/v1/memory/{mem}/revisions', { mem: sid }, {
    query: { limit: '1', cursor: '1' },
  });
  await t.call('memory lock', 'PATCH', '/v1/memory/{mem}', { mem: sid }, { ifMatch: '"r2"', body: { action: 'lock' } });
  await t.call('memory edit locked (service)', 'PATCH', '/v1/memory/{mem}', { mem: sid }, {
    token: t.service,
    ifMatch: '"r3"',
    body: { action: 'edit', entry: { title: 'Agent fact', body_markdown: 'Locked.', tags: [], step_id: 'check' } },
  });
  await t.call('memory unlock', 'PATCH', '/v1/memory/{mem}', { mem: sid }, { ifMatch: '"r3"', body: { action: 'unlock' } });
  await t.call('memory restore (service)', 'PATCH', '/v1/memory/{mem}', { mem: sid }, {
    token: t.service,
    ifMatch: '"r4"',
    body: { action: 'restore', revision: 1 },
  });
  await t.call('memory restore archived', 'PATCH', '/v1/memory/{mem}', { mem: mem.body.id }, {
    ifMatch: '"r2"',
    body: { action: 'restore', revision: 1 },
  });
  const proposal = await t.call('memory propose (service)', 'POST', '/v1/runbooks/{rb}/memory', { rb }, {
    token: t.service,
    body: { title: 'Maybe', body_markdown: 'The dashboard moved.', tags: [], step_id: 'check', propose: true },
  });
  const pid = proposal.body?.id ?? 'mem_00000000000000000000000000';
  await t.call('memory accept (service)', 'PATCH', '/v1/memory/{mem}', { mem: pid }, {
    token: t.service,
    ifMatch: '"r1"',
    body: { action: 'accept' },
  });
  await t.call('memory proposals (service)', 'GET', '/v1/runbooks/{rb}/memory', { rb }, {
    token: t.service,
    query: { status: 'proposed' },
  });
  await t.call('memory proposal read (service)', 'GET', '/v1/memory/{mem}', { mem: pid }, { token: t.service });
  await t.call('memory proposals', 'GET', '/v1/runbooks/{rb}/memory', { rb }, { query: { status: 'proposed' } });
  await t.call('memory unknown step', 'POST', '/v1/runbooks/{rb}/memory', { rb }, {
    body: { title: 'Nowhere', body_markdown: 'No such step.', tags: [], step_id: 'nope' },
  });
  await t.call('memory reject', 'PATCH', '/v1/memory/{mem}', { mem: pid }, {
    ifMatch: '"r1"',
    body: { action: 'reject' },
  });

  // ---- A run through every step-run route.
  const started = await t.call('start', 'POST', '/v1/projects/{p}/executions', { p: project }, {
    body: { runbook: 'contract', title: 'Contract run', tags: ['contract'] },
  });
  const exe = started.body.id;
  await t.call('start replay', 'POST', '/v1/projects/{p}/executions', { p: project }, {
    body: { runbook: 'contract', title: 'Contract run', tags: ['contract'] },
    key: 'start',
  });
  // Natively the drain worker activates a new run moments after the start;
  // the run's object does it inside the start request.
  // UI-2: a waiting run shows its top-level progress and what the wait awaits.
  await t.until(
    'execution',
    'GET',
    '/v1/executions/{exe}',
    { exe },
    (r) =>
      r.body?.status === 'waiting' &&
      typeof r.body?.progress?.total === 'number' &&
      (r.body?.steps ?? []).some((s) => s.wait?.signal === 'go'),
  );
  await t.events('events', exe);
  await t.call('next before signal', 'GET', '/v1/executions/{exe}/next', { exe });
  await t.call('signal', 'POST', '/v1/executions/{exe}/signals/{name}', { exe, name: 'go' }, { body: { payload: {} } });
  const ready = await t.until('next', 'GET', '/v1/executions/{exe}/next', { exe }, (r) =>
    JSON.stringify(r.body ?? {}).includes('check'),
  );
  await t.call('execution patch', 'PATCH', '/v1/executions/{exe}', { exe }, { body: { tags: ['contract', 'cf9'] } });
  await t.until('executions', 'GET', '/v1/projects/{p}/executions', { p: project }, (r) =>
    r.body?.items?.some((i) => (i.tags ?? []).includes('cf9')),
  );
  const steps = (await t.call('execution after signal', 'GET', '/v1/executions/{exe}', { exe })).body;
  const stepOf = (path) => (steps?.steps ?? []).find((s) => s.path === path)?.id;
  const check = stepOf('check') ?? JSON.stringify(ready.body).match(/sr_[0-9A-Z]{26}/)?.[0];
  await t.call('step run', 'GET', '/v1/step-runs/{sr}', { sr: check });
  // Execution credentials (D32) while the run is open: mint, refresh by
  // the refresh token alone, revoke the family.
  const minted2 = await t.call('execution token', 'POST', '/v1/executions/{exe}/tokens', { exe }, {
    body: { session_id: 'contract-agent' },
  });
  await t.call('execution token refresh', 'POST', '/v1/executions/{exe}/tokens/refresh', { exe }, {
    body: { refresh_token: minted2.body?.refresh_token ?? 'trb_refresh_none' },
    token: null,
  });
  await t.call('credential revoke', 'DELETE', '/v1/executions/{exe}/credentials/{family}', {
    exe,
    family: minted2.body?.family_id ?? 'fam_00000000000000000000000000',
  });

  const claimed = await t.call('claim', 'POST', '/v1/step-runs/{sr}/claim', { sr: check }, {
    token: t.service,
    body: { session_id: 'contract', actor: { kind: 'agent', agent_name: 'contract-agent' } },
  });
  const claim = { id: claimed.body.claim?.id ?? claimed.body.id, generation: claimed.body.claim?.generation ?? claimed.body.generation };
  await t.call('heartbeat', 'POST', '/v1/step-runs/{sr}/heartbeat', { sr: check }, { token: t.service, body: { claim } });
  await t.call('logs append', 'POST', '/v1/step-runs/{sr}/logs', { sr: check }, {
    token: t.service,
    body: { claim, logs: ['checking', 'ok'] },
  });
  await t.call('logs', 'GET', '/v1/step-runs/{sr}/logs', { sr: check });
  await t.call('skill files', 'GET', '/v1/step-runs/{sr}/skill-files', { sr: check }, {
    token: t.service,
    query: { path: 'SKILL.md' },
  });
  await t.call('report', 'POST', '/v1/step-runs/{sr}/report', { sr: check }, {
    token: t.service,
    body: { step_run_id: check, claim, status: 'succeeded', outputs: { code: 200 }, summary: 'Healthy' },
  });
  await t.call('override', 'POST', '/v1/step-runs/{sr}/overrides', { sr: check }, {
    body: {
      to: { status: 'succeeded', verdict: null, outputs: { code: 201 }, summary: 'Corrected' },
      reason: 'The first reading was wrong',
    },
  });
  const after2 = await t.until('execution at the list', 'GET', '/v1/executions/{exe}', { exe }, (r) =>
    JSON.stringify(r.body ?? {}).includes('"tick"'),
  );
  const tick = (after2.body?.steps ?? []).find((s) => s.path === 'tick')?.id;
  // UI-2: each task carries since when it has been open.
  await t.until('my tasks', 'GET', '/v1/me/tasks', {}, (r) =>
    (r.body?.items ?? []).some((i) => i.step_run_id === tick && typeof i.since_ms === 'number'),
  );
  const mine = await t.call('claim human', 'POST', '/v1/step-runs/{sr}/claim', { sr: tick }, {
    body: { session_id: 'contract-web' },
  });
  const human = { id: mine.body.claim?.id ?? mine.body.id, generation: mine.body.claim?.generation ?? mine.body.generation };
  await t.call('release', 'POST', '/v1/step-runs/{sr}/release', { sr: tick }, { body: { claim: human } });
  const again2 = await t.call('claim human again', 'POST', '/v1/step-runs/{sr}/claim', { sr: tick }, {
    body: { session_id: 'contract-web' },
    key: 'claim-again',
  });
  const human2 = { id: again2.body.claim?.id ?? again2.body.id, generation: again2.body.claim?.generation ?? again2.body.generation };
  const slot = await t.call('evidence slot', 'POST', '/v1/step-runs/{sr}/evidence', { sr: tick }, {
    body: { name: 'check.log', type: 'text/plain', size: 9 },
  });
  if (slot.status === 201) {
    const put = await fetch(slot.body.upload_url, {
      method: 'PUT',
      headers: { 'content-type': 'text/plain' },
      body: 'status ok',
    });
    t.note('evidence put', put.status);
  }
  await t.call('evidence finalize', 'POST', '/v1/step-runs/{sr}/evidence/{evd}/finalize', {
    sr: tick,
    evd: slot.body?.evidence_id ?? 'evd_00000000000000000000000000',
  }, { body: {} });
  await t.call('evidence', 'GET', '/v1/executions/{exe}/evidence', { exe });
  await t.call('checklist', 'POST', '/v1/step-runs/{sr}/checklist', { sr: tick }, {
    body: {
      claim: human2,
      checklist: [
        { title: 'First', completed: true },
        { title: 'Second', completed: true },
      ],
    },
  });
  await t.call('report human', 'POST', '/v1/step-runs/{sr}/report', { sr: tick }, {
    body: { step_run_id: tick, claim: human2, status: 'succeeded' },
  });
  const at = await t.until('execution at the approval', 'GET', '/v1/executions/{exe}', { exe }, (r) =>
    JSON.stringify(r.body ?? {}).includes('"decide"'),
  );
  const decide = (at.body?.steps ?? []).find((s) => s.path === 'decide')?.id;
  await t.call('decision', 'POST', '/v1/step-runs/{sr}/decisions', { sr: decide }, {
    body: { option: 'keep', comment: 'Looks good' },
  });
  await t.until('execution done', 'GET', '/v1/executions/{exe}', { exe }, (r) =>
    ['succeeded', 'failed'].includes(r.body?.status),
  );
  // UI-1: the runbook list's run fields once the run index has the
  // finished run (D56), and its server-side search.
  await t.until('runbooks after run', 'GET', '/v1/projects/{p}/runbooks', { p: project }, (r) =>
    (r.body?.items ?? []).some(
      (i) => i.last_run?.id === exe && ['succeeded', 'failed'].includes(i.last_run?.status) && i.active_runs === 0,
    ),
  );
  await t.call('runbooks search', 'GET', '/v1/projects/{p}/runbooks', { p: project }, {
    query: { q: 'SUITE', tag: 'cf9,contract', limit: '1' },
  });
  await t.call('runbooks search none', 'GET', '/v1/projects/{p}/runbooks', { p: project }, { query: { tag: 'CF9' } });
  await t.call('report json', 'GET', '/v1/executions/{exe}/report', { exe });
  await t.call('report markdown', 'GET', '/v1/executions/{exe}/report', { exe }, { query: { format: 'markdown' } });
  await t.call('archive', 'GET', '/v1/executions/{exe}/archive', { exe });
  const rerun = await t.call('rerun', 'POST', '/v1/executions/{exe}/rerun', { exe }, { body: {} });
  // UI-1: an open run counts as active, and a draft edited past the latest
  // version is ahead of it. As for the first run, wait for the activated
  // run at its gate (natively the drain activates it moments later).
  await t.call('draft ahead', 'PATCH', '/v1/runbooks/{rb}/draft', { rb }, {
    ifMatch: (await t.call('draft before ahead', 'GET', '/v1/runbooks/{rb}/draft', { rb })).headers.get('etag'),
    body: { source: source.replace('title: Contract run', 'title: Contract run v3') },
  });
  await t.until('runbooks while open', 'GET', '/v1/projects/{p}/runbooks', { p: project }, (r) =>
    (r.body?.items ?? []).some((i) => i.last_run?.id === rerun.body?.id && i.last_run?.status === 'waiting' && i.active_runs === 1 && i.draft_ahead),
  );
  await t.call('runbook archives', 'GET', '/v1/runbooks/{rb}/archives', { rb });
  await t.call('runbook archive day', 'GET', '/v1/runbooks/{rb}/archives/{date}', { rb, date: '2026-01-01' });

  await t.call('webhook delete', 'DELETE', '/v1/projects/{p}/webhooks/{id}', { p: project, id });

  // ---- LANG4: a `call` step runs another runbook as a child execution.
  // The child starts through the parent's work item (natively the drain
  // worker, on Cloudflare the parent's object), so reads wait with
  // `t.until` for the link both ways.
  await callScenario(t, project);
  await overlayScenario(t, project);

  // ---- LANG7: strict assignment and take-over.
  await lang7Scenario(t, project);
  await triggerScenario(t, project);

  // ---- Device login: sign-in is off on both builds (404).
  await t.call('device code', 'POST', '/v1/device/code', {}, { token: null, body: {} });
  await t.call('device token', 'POST', '/v1/device/token', {}, {
    token: null,
    body: { device_code: 'x', grant_type: 'urn:ietf:params:oauth:grant-type:device_code' },
  });
  await t.call('device approve', 'POST', '/v1/device/approve', {}, { body: { user_code: 'ABCD-EFGH', approve: true } });
  await t.call('device lookup', 'POST', '/v1/device/lookup', {}, { body: { user_code: 'ABCD-EFGH' } });

  // ---- Errors every route shares.
  await t.call('unknown run', 'GET', '/v1/executions/{exe}', { exe: 'exe_00000000000000000000000000' });
  await t.call('no token', 'GET', '/v1/projects/{p}/runbooks', { p: project }, { token: null });
  await t.call('bad body', 'POST', '/v1/projects/{p}/runbooks', { p: project }, { body: { nope: 1 } });
}
