// The same runner HTTP contract runs on Postgres and the Workers build.
import assert from 'node:assert/strict';
import { createHash, createPublicKey, generateKeyPairSync, verify } from 'node:crypto';
import { setTimeout } from 'node:timers/promises';

export function runnerClient(send) {
  let seq = 0;
  async function call(method, path, status, { token, body, headers, raw = false } = {}) {
    const response = await send(path, {
      method,
      headers: {
        ...(token && { authorization: `Bearer ${token}` }),
        ...(body && { 'content-type': 'application/json' }),
        ...(method === 'POST' && { 'idempotency-key': `runner-test-${++seq}` }),
        ...headers,
      },
      body: raw || body === undefined ? body : JSON.stringify(body),
    });
    const text = await response.text();
    assert.equal(response.status, status, `${method} ${path}: ${text}`);
    return { body: raw ? text : text ? JSON.parse(text) : null, headers: response.headers };
  }
  return call;
}

function verifyAssertion(jwks, jwt, expectedType) {
  const [header, payload, sig] = jwt.split('.');
  const decode = (part) => JSON.parse(Buffer.from(part, 'base64url'));
  const { alg, kid, typ } = decode(header);
  assert.equal(alg, 'EdDSA');
  assert.equal(typ, expectedType ?? (kid.startsWith('grant-') ? 'rb-grant+jwt' : 'rb-user+jwt'));
  const key = jwks.keys.find((key) => key.kid === kid);
  assert.ok(key, `JWKS has key ${kid}`);
  assert.equal(key.alg, 'EdDSA');
  assert.equal(key.kty, 'OKP');
  assert.equal(key.crv, 'Ed25519');
  assert.equal(key.use, 'sig');
  assert.equal(key.d, undefined);
  assert.ok(verify(null, Buffer.from(`${header}.${payload}`), createPublicKey({ key, format: 'jwk' }), Buffer.from(sig, 'base64url')));
  return decode(payload);
}

export async function runnerScenario({ call, token, project, org, user, url, forwarded, setRunnerReply }) {
  const { body: meta } = await call('GET', '/v1/meta', 200);
  assert.equal(meta.runner_contract_version, 4);
  const runnerStatus = `/v1/orgs/${org}/runner`;
  const bootstrapPath = `${runnerStatus}/bootstrap-tokens`;
  const mint = async (name, scopes, extra = {}) => (await call('POST', '/v1/tokens', 201, {
    token, body: { name, scopes, ...extra },
  })).body.secret;
  const noRunner = await mint('No runner scopes', ['runbooks:read']);
  const reader = await mint('Runner reader', ['runner:read', 'runbooks:read']);
  const adminOnly = await mint('Runner administrator', ['runner:admin']);
  const actingAgent = await mint('Runner agent caller', ['runner:read', 'runner:admin'], { actor_kind: 'agent' });
  assert.equal((await call('GET', runnerStatus, 200, { token: reader })).body.status, 'none');
  await call('GET', runnerStatus, 403, { token: adminOnly });
  for (const bearer of [noRunner, reader, actingAgent]) {
    await call('POST', bootstrapPath, 403, { token: bearer, body: { url } });
  }
  await call('POST', '/v1/orgs/org_00000000000000000000000099/runner/bootstrap-tokens', 404, { token, body: { url } });
  for (const forbidden of ['https://metadata.google.internal', 'https://169.254.169.254', 'https://user:pass@runner.example', 'ftp://runner.example']) {
    const invalidUrl = await call('POST', bootstrapPath, 422, { token, body: { url: forbidden } });
    assert.match(invalidUrl.body.code, /^runner\.url_/);
  }
  const { body: bootstrap } = await call('POST', bootstrapPath, 201, { token, body: { url } });
  assert.match(bootstrap.token, /^trb_rbs_/);
  const identityKey = () => generateKeyPairSync('ed25519').publicKey.export({ format: 'jwk' }).x;
  const initialIdentity = identityKey();
  const register = { vault_identity_key: initialIdentity, url, version: '0.1.0', contract_version: 2 };
  for (const [body, status, code] of [
    [{ ...register, url: 'https://different.example' }, 403, 'runner.url_mismatch'],
    [{ ...register, contract_version: 99 }, 422, 'runner.contract_unsupported'],
  ]) {
    assert.equal((await call('POST', '/v1/runner-installs/register', status, {
      token: bootstrap.token, body,
    })).body.code, code);
  }
  const { body: install } = await call('POST', '/v1/runner-installs/register', 201, {
    body: register, token: bootstrap.token,
  });
  await call('POST', '/v1/runner-installs/register', 401, {
    body: register, token: bootstrap.token,
  });
  const duplicate = await call('POST', `/v1/orgs/${org}/runner/bootstrap-tokens`, 409, {
    token, body: { url },
  });
  assert.equal(duplicate.body.code, 'runner.already_registered');
  assert.match(install.install_id, /^ri_/);
  assert.match(install.directory_token, /^trb_rdi_/);
  assert.match(install.release_token, /^trb_rrl_/);
  assert.match(install.join_token, /^trb_rjn_/);
  assert.equal((await call('GET', runnerStatus, 200, { token })).body.vault_identity_key, initialIdentity);
  const replacementIdentity = identityKey();
  await call('PUT', `${runnerStatus}/vault-identity`, 403, {
    token: actingAgent, body: { public_key: replacementIdentity },
  });
  await call('PUT', `${runnerStatus}/vault-identity`, 204, {
    token, body: { public_key: replacementIdentity },
  });
  assert.equal((await call('GET', runnerStatus, 200, { token })).body.vault_identity_key, replacementIdentity);
  const registry = `/v1/runner-installs/${install.install_id}`;
  const unrelated = '/v1/runner-installs/ri_00000000000000000000000001';
  const group = 'rg_00000000000000000000000001';
  const joinBody = { name: 'worker', group_id: group, group_name: 'ops', version: '0.1.0' };
  const join = { token: install.join_token, body: joinBody };
  await call('POST', `${unrelated}/runners`, 403, join);
  const { body: original } = await call('POST', `${registry}/runners`, 201, join);
  const { body: runner } = await call('POST', `${registry}/runners`, 201, {
    ...join, body: { ...joinBody, version: '0.1.1' },
  });
  assert.match(runner.runner_id, /^rn_/);
  assert.match(runner.claim_token, /^trb_rcl_/);
  assert.equal(runner.runner_id, original.runner_id);
  assert.notEqual(runner.claim_token, original.claim_token);
  await call('POST', '/v1/step-runs/sr_00000000000000000000000001/claim', 401, {
    token: original.claim_token, body: {},
  });
  const directory = { token: install.directory_token };
  const registryStatus = (await call('GET', runnerStatus, 200, { token })).body;
  assert.deepEqual(registryStatus.runners.map(({ id, group_id }) => [id, group_id]), [[runner.runner_id, group]]);
  assert.deepEqual(registryStatus.groups, [{ id: group, name: 'ops', projects: [] }]);
  assert.deepEqual((await call('GET', `${registry}/groups`, 200, directory)).body, {
    install_id: install.install_id, groups: registryStatus.groups,
  });
  await call('GET', `${unrelated}/groups`, 403, directory);
  await call('GET', `${registry}/groups`, 403, { token: runner.claim_token });
  const heartbeatBody = { groups: [], runners: [] };
  for (const bearer of [token, runner.claim_token, install.release_token, install.join_token]) {
    await call('POST', `${registry}/heartbeat`, 403, { token: bearer, body: heartbeatBody });
  }
  for (const bearer of [install.directory_token, install.release_token, install.join_token, runner.claim_token]) {
    for (const path of [runnerStatus, `/v1/projects/${project}/runbooks`, '/v1/me', '/v1/tokens']) {
      await call('GET', path, 403, { token: bearer });
    }
  }
  await call('POST', `${unrelated}/heartbeat`, 403, { ...directory, body: heartbeatBody });
  await call('POST', `${registry}/heartbeat`, 401, {
    token: 'trb_rdi_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', body: heartbeatBody,
  });
  const agent = {
    agent_id: 'agt_1', name: 'checker', project_id: project, group_id: group,
    status: 'active', version_id: 'v1',
    ceiling: {
      profiles: ['deep'], profile_models: { deep: 'review-model-1' }, max_cost_usd: 1, max_tokens: 200000, timeout_s: 900,
      credentials: [
        { id: 'cred_dd', name: 'datadog', delivery: ['proxy'], label: 'read' },
        { id: 'cred_other', name: 'other', delivery: ['proxy'], label: 'read' },
        { id: 'cred_model', name: 'model', delivery: ['model'], label: 'read' },
      ],
      tools: [], hosts: [],
    },
  };
  await call('PUT', `${registry}/agents/agt_1`, 403, { token: install.directory_token, body: agent });
  await call('PUT', `/v1/orgs/${org}/runner/groups/${group}/projects`, 204, {
    token, body: { project_ids: [project] },
  });
  await call('PUT', `${registry}/agents/agt_1`, 204, { token: install.directory_token, body: agent });
  const groups = (await call('GET', `${registry}/groups`, 200, directory)).body.groups;
  assert.deepEqual(groups, [{ id: group, name: 'ops', projects: [project] }]);
  const unknownGroup = 'rg_00000000000000000000000099';
  await call('PUT', `${runnerStatus}/groups/${unknownGroup}/projects`, 404, { token, body: { project_ids: [project] } });
  await call('PUT', `${runnerStatus}/groups/${group}/projects`, 422, {
    token, body: { project_ids: ['prj_00000000000000000000000099'] },
  });
  await call('PUT', `${registry}/agents/agt_2`, 422, { ...directory, body: agent });
  const reserved = await call('PUT', `${registry}/agents/agt_2`, 422, {
    ...directory, body: { ...agent, agent_id: 'agt_2', name: 'external' },
  });
  assert.equal(reserved.body.code, 'directory.name_reserved');
  await call('PUT', `${registry}/agents/agt_1`, 403, {
    ...directory, body: { ...agent, group_id: 'rg_00000000000000000000000077' },
  });
  await call('PUT', `${registry}/agents/agt_1`, 403, { token: runner.claim_token, body: agent });
  await publish('offline-agent', [{ id: 'check', title: 'Check', actor: 'agent', agent: { name: agent.name } }], project);
  const proxy = `/v1/runners/${install.install_id}/proxy/runner/v1/groups`;
  const csrf = { token, headers: { 'x-runbook-csrf': '1', cookie: 'session=secret' } };
  for (const headers of [{}, { 'x-runbook-csrf': '0' }]) {
    assert.equal((await call('GET', proxy, 403, { token, headers })).body.code, 'auth.csrf');
  }
  assert.equal(forwarded.length, 0);
  await call('GET', proxy, 503, csrf);
  await call('POST', `${registry}/heartbeat`, 204, {
    ...directory, body: {
      groups: [{ id: group, name: 'Renamed ops' }],
      runners: [{ id: runner.runner_id, status: 'ready', running: 2, capacity: 8 }],
    },
  });
  const online = (await call('GET', runnerStatus, 200, { token })).body;
  assert.equal(online.status, 'online');
  assert.deepEqual(online.groups, [{ id: group, name: 'Renamed ops', projects: [project] }]);
  assert.equal(online.runners[0].running, 2);
  assert.equal(online.runners[0].capacity, 8);
  const answer = await call('GET', `${proxy}?limit=5`, 200, csrf);
  assert.deepEqual(answer.body, { groups: [] });
  assert.equal(answer.headers.get('set-cookie'), null);
  assert.equal(answer.headers.get('x-content-type-options'), 'nosniff');
  assert.equal(answer.headers.get('content-security-policy'), 'sandbox');
  assert.equal(forwarded.length, 1);
  assert.equal(forwarded[0].url.pathname, '/runner/v1/groups');
  assert.equal(forwarded[0].headers.has('cookie'), false);
  assert.equal(forwarded[0].headers.has('authorization'), false);
  const { body: initialJwks } = await call('GET', '/.well-known/jwks.json', 200);
  assert.equal(initialJwks.keys.length, 2);
  assert.deepEqual(initialJwks.keys.map(({ kid }) => kid.split('-')[0]).sort(), ['grant', 'user']);
  const { body: discovery } = await call('GET', '/.well-known/runbook', 200);
  assert.equal(new URL(discovery.jwks_uri).pathname, '/.well-known/jwks.json');
  const assertion = verifyAssertion(initialJwks, forwarded[0].headers.get('runbook-assertion'));
  assert.equal(assertion.htm, 'GET');
  assert.equal(assertion.htu, '/runner/v1/groups?limit=5');
  assert.equal(assertion.bh, createHash('sha256').update(forwarded[0].body).digest('base64url'));
  assert.deepEqual(assertion.scopes, ['runner:read', 'runner:admin']);
  assert.equal(assertion.principal, 'human');
  assert.equal(assertion.sponsor_id, undefined);
  await call('GET', proxy.replace('/groups', '/private'), 404, csrf);
  setRunnerReply(() => Response.redirect('https://other.example/', 302));
  await call('GET', proxy, 502, csrf);
  assert.equal(forwarded.length, 2);
  setRunnerReply(() => new Response('data: {}\n\n', { headers: { 'content-type': 'text/event-stream' } }));
  await call('GET', proxy, 502, csrf);
  const sse = await call('GET', proxy.replace('/groups', '/transcripts/sr_00000000000000000000000001'), 200, {
    ...csrf, raw: true,
  });
  assert.match(sse.headers.get('content-type'), /^text\/event-stream/);
  assert.equal(sse.body, 'data: {}\n\n');
  setRunnerReply(() => new Response('<p>not JSON</p>', { headers: { 'content-type': 'text/html' } }));
  assert.equal((await call('GET', proxy, 502, csrf)).body.code, 'runner.bad_response');
  await call('PUT', `${registry}/agents/agt_2`, 409, {
    token: install.directory_token, body: { ...agent, agent_id: 'agt_2' },
  });

  setRunnerReply(() => Response.json({ created: true }));
  const updateGroup = { name: 'group proposal' };
  await call('POST', `${proxy}?dry_run=true`, 200, { ...csrf, body: updateGroup });
  const forwardedPost = forwarded.at(-1);
  const postAssertion = verifyAssertion(initialJwks, forwardedPost.headers.get('runbook-assertion'));
  assert.equal(postAssertion.htm, 'POST');
  assert.equal(postAssertion.htu, '/runner/v1/groups?dry_run=true');
  assert.deepEqual(JSON.parse(forwardedPost.body), updateGroup);
  assert.equal(postAssertion.bh, createHash('sha256').update(forwardedPost.body).digest('base64url'));
  assert.equal(postAssertion.aud, `runner:${install.install_id}`);
  assert.equal(postAssertion.org_id, org);
  assert.equal(postAssertion.user_id, user);
  assert.equal(postAssertion.project_roles[project], 'admin');
  assert.ok(postAssertion.exp > postAssertion.iat && postAssertion.exp - postAssertion.iat <= 60);
  assert.notEqual(postAssertion.jti, assertion.jti);
  const beforeDenied = forwarded.length;
  await call('GET', proxy, 403, { ...csrf, token: noRunner });
  await call('GET', proxy, 403, { ...csrf, token: adminOnly });
  await call('POST', proxy, 403, { ...csrf, token: reader, body: updateGroup });
  assert.equal(forwarded.length, beforeDenied);
  await call('GET', proxy, 200, { ...csrf, token: reader });
  assert.deepEqual(verifyAssertion(initialJwks, forwarded.at(-1).headers.get('runbook-assertion')).scopes, ['runner:read']);
  await call('GET', proxy, 200, { ...csrf, token: actingAgent });
  const agentAssertion = verifyAssertion(initialJwks, forwarded.at(-1).headers.get('runbook-assertion'));
  assert.equal(agentAssertion.principal, 'agent');
  assert.equal(agentAssertion.sponsor_id, user);
  await call('POST', proxy, 200, { ...csrf, token: adminOnly, body: updateGroup });
  setRunnerReply(() => Response.json({ code: 'agent.not_found' }, {
    status: 404, headers: { 'content-type': 'application/json; charset=utf-8' },
  }));
  const missingAgent = await call('GET', proxy.replace('/groups', `/projects/${project}/agents/agt_9`), 404, csrf);
  assert.equal(missingAgent.body.code, 'agent.not_found');
  assert.equal(missingAgent.headers.get('x-content-type-options'), 'nosniff');
  assert.equal(missingAgent.headers.get('content-security-policy'), 'sandbox');
  assert.equal(missingAgent.headers.get('content-type'), 'application/json');
  setRunnerReply(() => Response.json({ groups: [] }));

  const task = (id) => ({
    id, title: id, actor: 'agent', connections: ['datadog'],
    agent: { name: 'checker', model: 'deep', max_cost_usd: 0.5, network: [] },
  });
  async function publish(slug, steps, projectId, status = 201, metadata = {}) {
    const source = JSON.stringify({
      apiVersion: 'therunbook.app/v1', kind: 'Runbook',
      metadata: { id: slug, title: slug, ...metadata }, spec: { steps },
    });
    const { body: runbook } = await call('POST', `/v1/projects/${projectId}/runbooks`, 201, { token, body: { source } });
    const rb = `/v1/runbooks/${runbook.runbook_id}`;
    const draft = await call('GET', `${rb}/draft`, 200, { token });
    const result = await call('POST', `${rb}/versions`, status, {
      token, body: {}, headers: { 'if-match': draft.headers.get('etag') },
    });
    return { id: runbook.runbook_id, result: result.body };
  }
  async function publishAndStart(slug, steps, projectId = project, metadata = {}) {
    const runbook = await publish(slug, steps, projectId, 201, metadata);
    const { body: run } = await call('POST', `/v1/projects/${projectId}/executions`, 201, {
      token, body: { runbook: runbook.id },
    });
    return run.id;
  }
  for (const [slug, options, diagnostic] of [
    ['no-such-agent', { name: 'ghost' }, 'spec.agent_unknown'],
    ['over-ceiling', { max_cost_usd: 5 }, 'spec.agent_outside_ceiling'],
  ]) {
    const check = task('check');
    const { result } = await publish(slug, [{ ...check, agent: { ...check.agent, ...options } }], project, 422);
    assert.ok(JSON.stringify(result).includes(diagnostic), JSON.stringify(result));
  }
  const run = await publishAndStart('checks', [{
    id: 'all',
    parallel: [task('first'), task('second'), { id: 'external', title: 'External', actor: 'agent' }],
  }]);
  const ready = `${registry}/ready?group=${group}&project=${project}&limit=1`;
  await call('GET', ready, 403, { token: runner.claim_token });
  let first;
  const deadline = Date.now() + 10_000;
  do {
    ({ body: first } = await call('GET', ready, 200, directory));
    if (first.next_cursor || Date.now() >= deadline) break;
    await setTimeout(40);
  } while (true);
  assert.equal(first.items.length, 1);
  assert.ok(first.next_cursor);
  const cursor = encodeURIComponent(first.next_cursor);
  const { body: other } = await call('GET', `${ready}&cursor=${cursor}`, 200, directory);
  assert.equal(other.items.length, 1);
  assert.notEqual(other.items[0].step_run_id, first.items[0].step_run_id);
  for (const item of [...first.items, ...other.items]) {
    assert.equal(item.execution_id, run);
    assert.equal(item.project_id, project);
    assert.equal(item.agent_id, agent.agent_id);
    assert.equal(item.max_cost_usd, 0.5);
  }
  await call('GET', `${registry}/ready?group=${group}&cursor=${cursor}`, 422, directory);
  const sr = first.items[0].step_run_id;
  await call('POST', `${registry}/waiting`, 204, {
    ...directory, body: { items: [{ step_run_id: sr, reason: 'limit:concurrency', detail: 'Project limit reached' }] },
  });
  const execution = `/v1/executions/${run}`;
  const step = async () => (await call('GET', execution, 200, { token })).body.steps.find(({ id }) => id === sr);
  assert.equal((await step()).runner.waiting_detail, 'Project limit reached');
  await call('PUT', `${registry}/agents/agt_1`, 204, { ...directory, body: { ...agent, name: 'renamed' } });
  assert.equal((await step()).runner.agent_id, agent.agent_id);
  assert.equal((await step()).runner.agent_name, agent.name);
  for (const agent_id of ['agt_2', 'agt_3']) {
    const path = `${registry}/agents/${agent_id}`;
    await call('PUT', path, 204, { ...directory, body: { ...agent, agent_id } });
    await call('DELETE', path, 204, directory);
  }
  await call('PUT', `${registry}/agents/agt_1`, 204, { ...directory, body: agent });
  const claimBody = { session_id: 'session', actor: { kind: 'agent', agent_name: 'checker' } };
  const claimStep = (id, status, bearer = runner.claim_token) =>
    call('POST', `/v1/step-runs/${id}/claim`, status, { token: bearer, body: claimBody });
  await claimStep(sr, 403, install.directory_token);
  const externalAgent = await mint('External actor', ['executions:act', 'executions:read'], { actor_kind: 'agent' });
  assert.equal((await claimStep(sr, 403, externalAgent)).body.code, 'claim.wrong_runner');
  const { body: currentRun } = await call('GET', execution, 200, { token });
  const external = currentRun.steps.find(({ path }) => path === 'external');
  assert.deepEqual([sr, other.items[0].step_run_id].sort(),
    currentRun.steps.filter(({ path }) => ['first', 'second'].includes(path)).map(({ id }) => id).sort());
  assert.equal((await step()).runner.waiting_reason, 'limit:concurrency');
  await call('POST', `${registry}/waiting`, 422, {
    ...directory, body: { items: [{ step_run_id: sr, reason: 'because' }] },
  });
  await call('POST', `${registry}/waiting`, 204, {
    ...directory, body: { items: [{ step_run_id: external.id, reason: 'runner_offline' }] },
  });
  const afterWaiting = (await call('GET', execution, 200, { token })).body;
  assert.equal(afterWaiting.steps.find(({ id }) => id === external.id).runner ?? null, null);
  assert.equal((await claimStep(external.id, 403)).body.code, 'claim.wrong_runner');
  const { body: externalClaim } = await claimStep(external.id, 200, externalAgent);
  await call('POST', `/v1/step-runs/${external.id}/report`, 200, {
    token: externalAgent,
    body: { step_run_id: external.id, claim: externalClaim, status: 'succeeded', summary: 'External check passed' },
  });
  assert.equal((await call('GET', execution, 200, { token })).body.cost, undefined);
  await claimStep('sr_00000000000000000000000077', 404);
  const otherGroup = 'rg_00000000000000000000000002';
  const { body: outsider } = await call('POST', `${registry}/runners`, 201, {
    token: install.join_token,
    body: { name: 'outside', group_id: otherGroup, group_name: 'Other', version: '0.1.0' },
  });
  assert.equal((await claimStep(sr, 403, outsider.claim_token)).body.code, 'claim.wrong_runner');
  assert.deepEqual((await call('GET', `${registry}/ready?group=${otherGroup}`, 200, directory)).body.items, []);
  const { body: claim } = await claimStep(sr, 200);
  await call('PUT', `${registry}/agents/agt_1`, 204, {
    ...directory, body: { ...agent, version_id: 'v2' },
  });
  const context = (stepId, binding, action, query = {}) =>
    `/v1/step-runs/${stepId}/context/${action}?${new URLSearchParams({
      claim_id: binding.id, generation: binding.generation, ...query,
    })}`;
  const claimAuth = { token: runner.claim_token };
  const heartbeat = (binding, auth, status) => call('POST', `/v1/step-runs/${sr}/heartbeat`, status, {
    ...auth, body: { claim: binding },
  });
  const { body: leaseDeadline } = await heartbeat(claim, claimAuth, 200);
  await call('GET', `/v1/step-runs/${sr}`, 200, claimAuth);
  const overview = context(sr, claim, 'run');
  const { body: current } = await call('GET', overview, 200, claimAuth);
  assert.equal(current.id, run);
  assert.equal(current.runbook_slug, 'checks');
  assert.equal(current.project_id, project);
  assert.equal(current.previous_attempt, undefined);
  await call('GET', overview, 403, directory);
  await call('GET', overview, 403, { token });
  await call('GET', overview, 403, { token: outsider.claim_token });
  await call('GET', context(sr, claim, 'step', { step_run_id: sr }), 404, claimAuth);
  await call('GET', context(other.items[0].step_run_id, claim, 'run'), 403, claimAuth);
  await call('GET', context(sr, { ...claim, generation: claim.generation + 1 }, 'run'), 403, claimAuth);
  await call('POST', context(sr, claim, 'memory'), 201, {
    ...claimAuth, body: { title: 'Proposed baseline', body_markdown: 'An observation', tags: [] },
  });
  for (let i = 1; i < 6; i++) {
    await call('POST', context(sr, claim, 'memory'), i === 5 ? 409 : 201, {
      ...claimAuth, body: { title: `Baseline ${i}`, body_markdown: `Observation ${i}` },
    });
  }
  const { body: activeMemory } = await call('GET', context(sr, claim, 'memory'), 200, claimAuth);
  assert.deepEqual(activeMemory.items, []);

  const pinned = (await step()).runner;
  assert.equal(pinned.agent_version_id, 'v1');
  assert.equal(pinned.waiting_reason ?? null, null);
  const { body: afterClaim } = await call('GET', ready, 200, directory);
  assert.deepEqual(afterClaim.items.map(({ step_run_id: id }) => id), [other.items[0].step_run_id]);

  const grantBody = {
    kind: 'credential', credential_id: 'cred_dd', credential_version: 1, delivery: 'proxy',
    claim_id: claim.id, generation: claim.generation, runner_id: runner.runner_id,
  };
  const grantRoute = `/v1/step-runs/${sr}/grants`;
  const requestGrant = () => call('POST', grantRoute, 200, { token: install.release_token, body: grantBody });
  const { body: result } = await requestGrant();
  await requestGrant();
  assert.equal((await step()).runner.releases.length, 1);
  assert.equal((await step()).runner.releases[0].delivery, 'proxy');
  assert.equal((await step()).runner.releases[0].credential_id, 'cred_dd');
  assert.equal((await step()).runner.releases[0].credential_version, 1);
  assert.equal((await step()).runner.releases[0].runner_id, runner.runner_id);
  const { body: jwks } = await call('GET', '/.well-known/jwks.json', 200);
  const claims = verifyAssertion(jwks, result.grant);
  assert.equal(result.expires_at, leaseDeadline);
  assert.equal(claims.exp, Math.floor(result.expires_at / 1000));
  assert.deepEqual(claims.network, []);
  assert.deepEqual(claims.connections, ['cred_dd']);
  assert.equal(claims.destructive, false);
  assert.equal(claims.model_profile, 'deep');
  assert.equal(claims.agent_version, 'v1');
  assert.equal(claims.aud, `runner:${runner.runner_id}`);
  assert.equal(claims.sub, sr);
  assert.equal(claims.group_id, group);
  assert.equal(claims.project_id, project);
  assert.equal(claims.credential_id, 'cred_dd');
  assert.equal(claims.credential_version, 1);
  const lease = { claim_id: claim.id, generation: claim.generation, runner_id: runner.runner_id };
  const capabilityRequest = { kind: 'capability', ...lease };
  const putAgent = (entry) => call('PUT', `${registry}/agents/agt_1`, 204, {
    ...directory, body: entry,
  });
  const grant = (body, status) => call('POST', grantRoute, status, {
    token: install.release_token, body,
  });
  for (const body of [
    { ...grantBody, credential_id: 'cred_other' },
    { ...grantBody, credential_id: 'cred_unknown' },
    { ...grantBody, delivery: 'token' },
  ]) {
    assert.equal((await grant(body, 403)).body.code, 'grant.not_allowed');
  }
  await call('POST', grantRoute, 403, { ...claimAuth, body: capabilityRequest });
  await grant({ ...capabilityRequest, runner_id: 'rn_00000000000000000000000055' }, 403);
  assert.equal(
    (await grant({ ...capabilityRequest, runner_id: outsider.runner_id }, 409)).body.code,
    'grant.claim_moved',
  );
  for (const entry of [
    { ...agent, status: 'paused' },
    { ...agent, ceiling: { ...agent.ceiling, max_cost_usd: 0.25 } },
  ]) {
    await putAgent({ ...entry, version_id: 'v3' });
    assert.equal((await grant(capabilityRequest, 403)).body.code, 'grant.not_allowed');
    assert.equal((await grant(grantBody, 403)).body.code, 'grant.not_allowed');
  }
  await putAgent({ ...agent, version_id: 'v2' });
  await grant(capabilityRequest, 200);
  assert.equal((await step()).runner.releases.length, 1);

  const { body: subscription } = await call('POST', `/v1/projects/${project}/webhooks`, 201, {
    token, body: { name: 'Runner budgets', url, events: ['runner.budget_threshold'] },
  });
  await call('POST', `${registry}/events`, 422, {
    ...directory, body: { kind: 'runner.unknown', data: {} },
  });
  await call('POST', `${registry}/events`, 204, {
    ...directory, body: { kind: 'runner.budget_threshold', data: { level: 'project', percent: 80 } },
  });
  const deliveries = `/v1/projects/${project}/webhooks/${subscription.webhook.id}/deliveries`;
  assert.deepEqual(
    (await call('GET', deliveries, 200, { token })).body.items.map(({ event_type }) => event_type),
    ['runner.budget_threshold'],
  );
  const release = { token: install.release_token };
  const { body: capability } = await call('POST', grantRoute, 200, {
    ...release, body: { kind: 'capability', claim_id: claim.id, generation: claim.generation, runner_id: runner.runner_id },
  });
  const policy = verifyAssertion(jwks, capability.grant);
  assert.deepEqual(policy.network, []);
  assert.equal(policy.credential_id, undefined);
  assert.equal(policy.credential_version, undefined);
  assert.equal(policy.delivery, undefined);
  const invalidGrant = await call('POST', grantRoute, 422, {
    ...release, body: { ...grantBody, connections: ['injected'] },
  });
  assert.equal(invalidGrant.body.code, 'request.invalid');

  const groupProjects = `/v1/orgs/${org}/runner/groups/${group}/projects`;
  await call('PUT', groupProjects, 204, { token, body: { project_ids: [] } });
  assert.deepEqual((await call('GET', ready, 200, directory)).body.items, []);
  assert.equal((await claimStep(other.items[0].step_run_id, 403)).body.code, 'claim.wrong_runner');
  const removed = await publish('removed-agent', [task('check')], project, 422);
  assert.ok(JSON.stringify(removed.result).includes('spec.agent_unknown'));
  assert.equal((await call('POST', grantRoute, 403, { ...release, body: grantBody })).body.code, 'grant.not_allowed');
  const revokeBody = { credential_id: 'cred_dd', reason: 'grant.not_allowed' };
  for (const reason of ['', 'Lease ended', 'lease ended', '1lease', 'lease-ended', 'lease_ended\n', '<b>hi</b>', 'a'.repeat(65)]) {
    await call('POST', `${grantRoute}/revoked`, 422, { ...release, body: { ...revokeBody, reason } });
  }
  assert.equal((await step()).runner.releases[0].revoked, undefined);
  await call('POST', `${grantRoute}/revoked`, 204, { ...release, body: revokeBody });
  assert.equal((await step()).runner.releases[0].revoked, 'grant.not_allowed');
  await call('PUT', groupProjects, 204, { token, body: { project_ids: [project] } });
  const nextVersion = { ...grantBody, credential_version: 2 };
  await grant(nextVersion, 200);
  assert.deepEqual((await step()).runner.releases.map(({ credential_version }) => credential_version), [1, 2]);
  const longestReason = 'a'.repeat(64);
  await call('POST', `${grantRoute}/revoked`, 204, { ...release, body: { ...revokeBody, reason: longestReason } });
  await grant(nextVersion, 200);
  const releases = (await step()).runner.releases;
  assert.deepEqual(releases.map(({ credential_version }) => credential_version), [1, 2, 2]);
  assert.equal(releases[1].revoked, longestReason);
  assert.equal(releases[2].revoked, undefined);
  const disallowedGrant = await call('POST', grantRoute, 403, {
    ...release, body: { ...grantBody, credential_id: 'cred_other' },
  });
  assert.equal(disallowedGrant.body.code, 'grant.not_allowed');

  const reportPath = `/v1/step-runs/${sr}/report`;
  const reportBody = { step_run_id: sr, claim, status: 'succeeded' };
  for (const cost of [{ model_usd: -1 }, { compute_usd: -1 }, { model_usd: 'NaN' }, { input_tokens: -5 }]) {
    await call('POST', reportPath, 422, { ...claimAuth, body: { ...reportBody, cost } });
  }
  const invalidCost = JSON.stringify({ ...reportBody, cost: { model_usd: 1000 } }).replace('"model_usd":1000', '"model_usd":1e400');
  await call('POST', reportPath, 422, { ...claimAuth, body: invalidCost, raw: true });
  await call('POST', `/v1/step-runs/${sr}/report`, 200, {
    token: runner.claim_token,
    body: {
      step_run_id: sr, claim, status: 'succeeded', verdict: 'pass', summary: 'Done',
      cost: { model_usd: 0.012345, compute_usd: 0.000001, input_tokens: 10, output_tokens: 3, cached_tokens: 6 },
    },
  });
  await call('GET', overview, 403, claimAuth);
  const reported = await step();
  assert.equal(reported.status, 'succeeded');
  assert.equal(reported.cost.model_usd, 0.012345);
  assert.equal(reported.cost.compute_usd, 0.000001);
  assert.equal(reported.cost.cached_tokens, 6);
  const { body: finished } = await call('GET', execution, 200, { token });
  assert.equal(finished.cost.model_usd, 0.012345);
  await call('POST', grantRoute, 409, { ...release, body: grantBody });

  const second = other.items[0].step_run_id;
  const otherStep = async () =>
    (await call('GET', execution, 200, { token })).body.steps.find(({ id }) => id === second);
  await putAgent({ ...agent, status: 'paused' });
  assert.equal((await claimStep(second, 409)).body.code, 'claim.agent_paused');
  assert.equal((await otherStep()).runner.waiting_reason, 'agent_paused');
  await putAgent({ ...agent, ceiling: { ...agent.ceiling, max_cost_usd: 0.1 } });
  assert.equal((await claimStep(second, 409)).body.code, 'claim.outside_ceiling');
  assert.equal((await otherStep()).runner.waiting_reason, 'agent_outside_ceiling');
  await putAgent(agent);
  const secondClaim = (await claimStep(second, 200)).body;
  const { body: details } = await call('GET', context(second, secondClaim, 'step', { step_run_id: sr }), 200, claimAuth);
  assert.equal(details.summary, 'Done');
  assert.equal(details.status, 'succeeded');
  const secondGrant = { ...grantBody, claim_id: secondClaim.id, generation: secondClaim.generation };
  const secondRoute = `/v1/step-runs/${second}/grants`;
  await call('POST', secondRoute, 200, { ...release, body: secondGrant });
  assert.equal((await otherStep()).runner.released_never_reported, undefined);
  await call('POST', `/v1/step-runs/${second}/release`, 200, { token, body: { reason: 'Taking over' } });
  assert.equal((await otherStep()).runner.released_never_reported, true);
  const { body: humanClaim } = await call('POST', `/v1/step-runs/${second}/claim`, 200, {
    token, body: { session_id: 'person', take_over: true, actor: { kind: 'human' } },
  });
  assert.equal(
    (await call('POST', secondRoute, 409, { ...release, body: secondGrant })).body.code,
    'grant.claim_moved',
  );
  for (const generation of [secondClaim.generation, 99]) {
    const stale = { ...capabilityRequest, claim_id: secondClaim.id, generation };
    assert.equal((await call('POST', secondRoute, 409, { ...release, body: stale })).body.code, 'grant.claim_moved');
  }
  await call('GET', context(second, secondClaim, 'run'), 403, claimAuth);
  await call('POST', `/v1/step-runs/${second}/report`, 200, {
    token,
    body: {
      step_run_id: second, claim: humanClaim, status: 'succeeded', verdict: 'pass',
    },
  });
  const ledger = { model_usd: 0.003306, compute_usd: 0.000004, input_tokens: 42 };
  const taskCost = (step_run_id, claim_id, cost, status = 204, runner_id = runner.runner_id) =>
    call('POST', `${registry}/events`, status, {
      ...directory, body: { kind: 'runner.task_cost', data: { step_run_id, claim_id, runner_id, cost } },
    });
  await taskCost(second, secondClaim.id, ledger, 404, outsider.runner_id);
  await taskCost(second, secondClaim.id, ledger);
  await taskCost(second, secondClaim.id, ledger);
  // The report on the first claim already recorded its cost.
  await taskCost(sr, claim.id, { model_usd: 0.012345, input_tokens: 10 });
  await call('POST', `/v1/step-runs/${second}/report`, 403, {
    ...claimAuth, body: { step_run_id: second, claim: secondClaim, status: 'succeeded', cost: ledger },
  });
  const { cost: takenOverCost } = await otherStep();
  for (const [key, value] of Object.entries(ledger)) assert.equal(takenOverCost[key], value);
  const { body: totaled } = await call('GET', execution, 200, { token });
  assert.equal(totaled.cost.model_usd, 0.015651);
  assert.equal(totaled.cost.compute_usd, 0.000005);
  assert.equal(totaled.cost.input_tokens, 52);
  assert.equal(totaled.cost.output_tokens, 3);
  assert.equal(totaled.cost.cached_tokens, 6);

  const { body: team } = await call('POST', `/v1/orgs/${org}/teams`, 201, {
    token, body: { slug: 'oncall', title: 'On call' },
  });
  await call('POST', `/v1/orgs/${org}/teams/${team.id}/members`, 200, { token, body: { user_id: user } });
  const host = 'api.datadoghq.com';
  await putAgent({ ...agent, ceiling: { ...agent.ceiling, hosts: [{ host, label: 'read' }] } });
  const deploy = task('deploy');
  const gateDefinition = { prompt: 'Deploy?', options: ['go', 'stop'], approvers: { teams: ['oncall'] } };
  const destructiveTask = { ...deploy, destructive: true, agent: { ...deploy.agent, network: [host] } };
  const unsafe = await publish('unsafe-gated', [{ id: 'gate', approval: gateDefinition }, destructiveTask], project, 422);
  assert.ok(JSON.stringify(unsafe.result).includes('spec.destructive_approval_options'));
  const gated = await publishAndStart('gated', [
    { id: 'gate', approval: { ...gateDefinition, approves: ['go'] } }, destructiveTask,
  ]);
  const gatedPath = `/v1/executions/${gated}`;
  async function awaitRun(path, predicate) {
    // On Postgres, the drainer can retry a failed activation after its
    // 30-second work lease expires. Wait through one recovery cycle.
    const deadline = Date.now() + 45_000;
    do {
      const { body } = await call('GET', path, 200, { token });
      const found = predicate(body);
      if (found) return found;
      assert.ok(Date.now() < deadline,
        `waiting for execution ${path} (${body.status}, admission: ${body.admission?.state ?? 'none'}): ` +
        body.steps.map(({ path, status }) => `${path}=${status}`).join(', '));
      await setTimeout(100);
    } while (true);
  }
  const awaitStep = (path, predicate) => awaitRun(path, (body) => body.steps.find(predicate));
  const gate = await awaitStep(gatedPath, ({ path }) => path === 'gate');
  await call('POST', `/v1/step-runs/${gate.id}/decisions`, 200, { token, body: { option: 'go' } });
  const destructive = await awaitStep(gatedPath, ({ path, status }) => path === 'deploy' && status === 'ready');
  const { body: gatedClaim } = await claimStep(destructive.id, 200);
  assert.deepEqual((await call('GET', ready, 200, directory)).body.items, []);
  await call('POST', `/v1/step-runs/${destructive.id}/grants`, 422, {
    ...release, body: { ...capabilityRequest, claim_id: gatedClaim.id, generation: gatedClaim.generation,
      destructive: false, network: [] },
  });
  for (const kind of ['capability', 'credential']) {
    const { body } = await call('POST', `/v1/step-runs/${destructive.id}/grants`, 200, {
      ...release,
      body: {
        ...kind === 'credential' && grantBody, kind, claim_id: gatedClaim.id,
        generation: gatedClaim.generation, runner_id: runner.runner_id,
      },
    });
    const signed = verifyAssertion(jwks, body.grant);
    assert.equal(signed.destructive, true);
    assert.deepEqual(signed.network, [host]);
    assert.deepEqual(signed.connections, ['cred_dd']);
  }

  const review = (id, after, extra = {}) => ({
    id, approval: {
      prompt: 'Ship the release?', options: ['ship', 'hold'], approvers: { teams: ['oncall'] },
      allow_agents: true, agent: { name: 'checker', model: 'deep', max_cost_usd: 0.25,
        max_tokens: 1000, ...(after && { after }) }, ...extra,
    },
  });
  const leaseSubscription = (await call('POST', `/v1/projects/${project}/webhooks`, 201, {
    token, body: { name: 'Decision lease events', url, events: ['lease.revoked'] },
  })).body.webhook;
  const approvalRun = await publishAndStart('runner-approvals', [{
    id: 'approvals',
    parallel: [
      review('slow', '1h'), review('agent', '250ms'), review('person'), review('abstain'),
      review('automatic', undefined, { timeout: '3s', options: ['ship', { id: 'hold', auto: true }],
        on_timeout: { decide: 'hold' } }),
    ],
  }]);
  const approvalPath = `/v1/executions/${approvalRun}`;
  const approvalStep = (id) => awaitStep(approvalPath, ({ path }) => path === id);
  const oldWorker = (await call('POST', `${registry}/runners`, 201, {
    ...join, body: { ...joinBody, name: 'legacy-worker' },
  })).body;
  const decisionHeartbeat = (supported) => call('POST', `${registry}/heartbeat`, 204, {
    ...directory, body: { runners: [
      { id: runner.runner_id, status: 'ready', capacity: 1, running: 0, ...(supported && { decision_contract: 1 }) },
      { id: oldWorker.runner_id, status: 'ready', capacity: 1, running: 0 },
    ] },
  });
  const readyReviews = `${registry}/ready?group=${group}&project=${project}&limit=100`;
  await decisionHeartbeat(false);
  const ordinary = await call('GET', `${readyReviews}&decision_contract=1`, 200, directory);
  assert.ok(!ordinary.body.items.some(({ kind }) => kind === 'decision'));
  const waiting = await approvalStep('person');
  assert.equal((await claimStep(waiting.id, 409)).body.code, 'runner.decision_unsupported');
  await decisionHeartbeat(true);
  assert.equal((await claimStep(waiting.id, 409, oldWorker.claim_token)).body.code, 'runner.decision_unsupported');
  assert.ok(!(await call('GET', readyReviews, 200, directory)).body.items.some(({ kind }) => kind === 'decision'));
  const slow = await approvalStep('slow');
  assert.equal((await claimStep(slow.id, 409)).body.code, 'step.not_claimable');
  const externalDecision = { token: externalAgent, body: {
    option: 'hold', session_id: 'external-review', actor: { kind: 'agent', agent_name: 'external-reviewer' },
  } };
  assert.equal((await call('POST', `/v1/step-runs/${slow.id}/decisions`, 409, externalDecision)).body.code,
    'approval.agent_not_ready');
  const automated = await approvalStep('automatic');
  const automatedClaim = (await claimStep(automated.id, 200)).body;
  const reviewers = await approvalStep('agent');
  const listReady = async () => (await call('GET', `${readyReviews}&decision_contract=1`, 200, directory)).body.items;
  let queued;
  const readyUntil = Date.now() + 10_000;
  do {
    const rows = await listReady();
    assert.ok(!rows.some(({ step_run_id }) => step_run_id === slow.id));
    queued = rows.find(({ step_run_id }) => step_run_id === reviewers.id);
    if (!queued) await setTimeout(40);
  } while (!queued && Date.now() < readyUntil);
  assert.equal(queued?.kind, 'decision');
  assert.deepEqual(queued.decision.options.map(({ id }) => id), ['ship', 'hold']);
  assert.equal(queued.decision.prompt, 'Ship the release?');
  assert.equal(queued.max_cost_usd, 0.25);
  assert.equal(queued.step_policy, undefined);

  const decisionLease = (await claimStep(reviewers.id, 200)).body;
  await claimStep(reviewers.id, 409);
  assert.equal((await approvalStep('agent')).status, 'waiting');
  const document = async () => (await call('GET', `${approvalPath}/report`, 200, { token })).body;
  const card = async (path) => (await document()).sections.find((section) => section.path === path).approval;
  assert.deepEqual((await card('agent')).decision_lease, { lease_id: decisionLease.id, agent_name: 'checker' });
  const leaseGrant = (step, lease, body, status) => call('POST', `/v1/step-runs/${step.id}/grants`, status, {
    ...release, body: { claim_id: lease.id, generation: lease.generation, runner_id: runner.runner_id, ...body },
  });
  await decisionHeartbeat(false);
  assert.equal((await leaseGrant(reviewers, decisionLease, { kind: 'capability' }, 403)).body.code, 'grant.not_allowed');
  await decisionHeartbeat(true);
  const capabilityGrant = await leaseGrant(reviewers, decisionLease, { kind: 'capability' }, 200);
  const decisionPolicy = verifyAssertion(jwks, capabilityGrant.body.grant);
  assert.equal(decisionPolicy.reach, 'read');
  assert.equal(decisionPolicy.destructive, false);
  assert.equal(decisionPolicy.model_profile, 'deep');
  assert.equal(decisionPolicy.model_id, 'review-model-1');

  await putAgent({ ...agent, ceiling: { ...agent.ceiling, credentials: [
    ...agent.ceiling.credentials, { id: 'cred_write', name: 'writer', delivery: ['proxy'], label: 'write' },
  ], hosts: [{ host, label: 'read' }, { host: 'write.example.com', label: 'write' }] } });
  const narrowed = verifyAssertion(jwks, (await leaseGrant(reviewers, decisionLease, { kind: 'capability' }, 200)).body.grant);
  assert.deepEqual(narrowed.network, [host]);
  assert.ok(!narrowed.connections.includes('cred_write'));
  await leaseGrant(reviewers, decisionLease,
    { kind: 'credential', credential_id: 'cred_write', credential_version: 1, delivery: 'proxy' }, 403);
  await leaseGrant(reviewers, decisionLease,
    { kind: 'credential', credential_id: 'cred_dd', credential_version: 1, delivery: 'proxy' }, 200);
  await call('POST', `/v1/step-runs/${reviewers.id}/heartbeat`, 200, { ...claimAuth, body: { claim: decisionLease } });
  const heldReview = await call('GET', `/v1/step-runs/${reviewers.id}`, 200, claimAuth);
  assert.equal(heldReview.body.kind, 'decision');
  await call('GET', context(reviewers.id, decisionLease, 'run'), 200, claimAuth);
  await call('POST', context(reviewers.id, decisionLease, 'memory'), 403, {
    ...claimAuth, body: { title: 'Memory', body_markdown: 'Review evidence', tags: [] },
  });

  const finishReview = (step, lease, action, body, status, auth = claimAuth, key) =>
    call('POST', `/v1/runner/decisions/${lease.id}/${action}?step_run_id=${step.id}`, status,
      { ...auth, body, ...(key && { headers: { 'idempotency-key': key } }) });
  const decisionBody = { option: 'ship', comment: 'Canary met the threshold [call 1].', evidence: ['[call 1]'],
    model: 'worker-reported-model', cost: { model_usd: 0.012, input_tokens: 23 } };
  assert.equal((await finishReview(reviewers, decisionLease, 'decide', decisionBody, 404, { token: outsider.claim_token })).body.code, 'claim.not_found');
  assert.equal((await finishReview(reviewers, decisionLease, 'decide', { ...decisionBody, evidence: [] }, 422)).body.code, 'approval.invalid_evidence');
  assert.equal((await finishReview(reviewers, decisionLease, 'decide', { ...decisionBody, option: 'Ship' }, 422)).body.code, 'approval.invalid_option');
  const decided = (await finishReview(reviewers, decisionLease, 'decide', decisionBody, 200, claimAuth, 'finish-runner-decision')).body;
  assert.equal(decided.agent_name, 'checker');
  assert.equal(decided.principal, `runner:${runner.runner_id}`);
  assert.equal(decided.model, 'review-model-1');
  assert.equal(decided.session_id, decisionLease.id);
  assert.deepEqual(decided.evidence, ['[call 1]']);
  assert.equal(decided.evidence_source, 'runner_reported');
  assert.deepEqual((await finishReview(reviewers, decisionLease, 'decide', decisionBody, 200, claimAuth, 'finish-runner-decision')).body, decided);
  assert.equal((await finishReview(reviewers, decisionLease, 'decide', { ...decisionBody, comment: 'Changed' }, 409, claimAuth, 'finish-runner-decision')).body.code, 'request.idempotency_key_reused');
  assert.deepEqual((await card('agent')).decisions, [decided]);
  assert.equal((await approvalStep('agent')).cost, undefined);
  const acceptedCost = { kind: 'runner.decision_cost', data: {
    step_run_id: reviewers.id, lease_id: decisionLease.id, cost: { model_usd: 0.012, input_tokens: 23 },
  } };
  await call('POST', `${registry}/events`, 204, { ...directory, body: acceptedCost });
  assert.equal((await approvalStep('agent')).cost.model_usd, 0.012);
  assert.equal((await card('agent')).decision_lease, undefined);

  const person = await approvalStep('person');
  const personLease = (await claimStep(person.id, 200)).body;
  await call('POST', `/v1/step-runs/${person.id}/decisions`, 200, { token, body: { option: 'hold' } });
  assert.equal((await finishReview(person, personLease, 'decide', decisionBody, 409)).body.code, 'approval.not_open');
  assert.equal((await leaseGrant(person, personLease, { kind: 'capability' }, 409)).body.code, 'grant.claim_moved');
  assert.equal((await call('POST', `/v1/step-runs/${person.id}/heartbeat`, 409, { ...claimAuth, body: { claim: personLease } })).body.code, 'claim.stale_generation');
  assert.equal((await card('person')).decision_lease, undefined);
  const lostCost = { kind: 'runner.decision_cost', data: {
    step_run_id: person.id, lease_id: personLease.id, cost: { model_usd: 0.023, input_tokens: 17 },
  } };
  await call('POST', `${registry}/events`, 403, { ...claimAuth, body: lostCost });
  await call('POST', `${registry}/events`, 404, { ...directory, body: { ...lostCost, data: {
    ...lostCost.data, lease_id: decisionLease.id,
  } } });
  await call('POST', `${registry}/events`, 204, { ...directory, body: lostCost });
  await call('POST', `${registry}/events`, 204, { ...directory, body: lostCost });
  assert.equal((await approvalStep('person')).cost.model_usd, 0.023);


  await awaitStep(approvalPath, ({ path, status }) => path === 'automatic' && status === 'succeeded');
  const abstain = await approvalStep('abstain');
  const releasedLease = (await claimStep(abstain.id, 200)).body;
  await call('PATCH', approvalPath, 200, { token, body: { action: 'pause' } });
  assert.equal((await card('abstain')).decision_lease, undefined);
  assert.equal((await finishReview(abstain, releasedLease, 'decide', decisionBody, 409)).body.code, 'execution.paused');
  assert.equal((await leaseGrant(abstain, releasedLease, { kind: 'capability' }, 409)).body.code, 'grant.claim_moved');
  assert.equal((await call('POST', `/v1/step-runs/${abstain.id}/heartbeat`, 409,
    { ...claimAuth, body: { claim: releasedLease } })).body.code, 'claim.stale_generation');
  await call('PATCH', approvalPath, 200, { token, body: { action: 'resume' } });
  const nextLease = (await claimStep(abstain.id, 200)).body;
  assert.equal(nextLease.generation, releasedLease.generation + 1);
  const noteBody = { reason: 'No current canary data.', model: 'review-model-1' };
  const note = (await finishReview(abstain, nextLease, 'abstain', noteBody, 200, claimAuth, 'finish-abstain')).body;
  assert.deepEqual((await finishReview(abstain, nextLease, 'abstain', noteBody, 200, claimAuth, 'finish-abstain')).body, note);
  assert.equal(note.reason, noteBody.reason);
  assert.deepEqual((await card('abstain')).agent_notes, [note]);
  assert.deepEqual((await card('abstain')).decisions, []);
  assert.equal((await card('abstain')).status, 'waiting');
  await claimStep(abstain.id, 409);
  await call('POST', `/v1/step-runs/${abstain.id}/decisions`, 200, { token, body: { option: 'hold' } });
  await awaitStep(approvalPath, ({ path, status }) => path === 'automatic' && status === 'succeeded');
  assert.equal((await leaseGrant(automated, automatedClaim, { kind: 'capability' }, 409)).body.code, 'grant.claim_moved');

  await call('PATCH', approvalPath, 200, { token, body: { action: 'cancel' } });
  const cancelRun = await publishAndStart('cancel-review', [review('review')]);
  const cancelPath = `/v1/executions/${cancelRun}`;
  const cancelReview = await awaitStep(cancelPath, ({ path }) => path === 'review');
  const cancelLease = (await claimStep(cancelReview.id, 200)).body;
  await call('PATCH', cancelPath, 200, { token, body: { action: 'cancel' } });
  assert.equal((await leaseGrant(cancelReview, cancelLease, { kind: 'capability' }, 409)).body.code, 'grant.claim_moved');
  await putAgent({ ...agent, ceiling: { ...agent.ceiling, hosts: [{ host, label: 'read' }] } });

  const leaseDeliveries = `/v1/projects/${project}/webhooks/${leaseSubscription.id}/deliveries`;
  const untilRevoked = Date.now() + 10_000;
  for (;;) {
    const { body } = await call('GET', leaseDeliveries, 200, { token });
    if (body.items.length === 6) {
      assert.ok(body.items.every(({ event_type }) => event_type === 'lease.revoked'));
      break;
    }
    assert.ok(Date.now() < untilRevoked, `expected six lease revocation events, received ${body.items.length}`);
    await setTimeout(40);
  }

  const projectPath = `/v1/projects/${project}`;
  await call('PATCH', projectPath, 422, { token, body: { default_agent: 'unknown' } });
  await call('PATCH', projectPath, 200, { token, body: { default_agent: 'checker' } });
  const { body: oldRun } = await call('GET', execution, 200, { token });
  assert.equal(oldRun.steps.find(({ id }) => id === external.id).runner ?? null, null);
  const { body: foreign } = await call('POST', `/v1/orgs/${org}/projects`, 201, {
    token, body: { slug: 'other-ops', title: 'Other ops' },
  });
  const restricted = await mint('Restricted runner reader', ['runner:read'], { projects: [project] });
  await call('GET', proxy, 200, { ...csrf, token: restricted });
  const restrictedAssertion = verifyAssertion(initialJwks, forwarded.at(-1).headers.get('runbook-assertion'));
  assert.equal(restrictedAssertion.org_role, 'member');
  assert.deepEqual(Object.keys(restrictedAssertion.project_roles), [project]);
  await call('GET', proxy, 200, csrf);
  const ownerAssertion = verifyAssertion(initialJwks, forwarded.at(-1).headers.get('runbook-assertion'));
  assert.equal(ownerAssertion.org_role, 'owner');
  assert.deepEqual(Object.keys(ownerAssertion.project_roles).sort(), [project, foreign.id].sort());

  // Use real projections in two projects on the same group. A project filter
  // that accidentally leaks rows can now fail even when both projects are allowed.
  await call('PUT', groupProjects, 204, { token, body: { project_ids: [project, foreign.id] } });
  const otherAgent = { ...agent, agent_id: 'agt_foreign', name: 'other-checker', project_id: foreign.id };
  await call('PUT', `${registry}/agents/${otherAgent.agent_id}`, 204, { ...directory, body: otherAgent });
  const otherRun = await publishAndStart('ready-other-project', [{
    ...task('other'), agent: { ...task('other').agent, name: otherAgent.name },
  }], foreign.id);
  const otherRunPath = `/v1/executions/${otherRun}`;
  const otherReady = await awaitStep(otherRunPath, ({ path, status }) => path === 'other' && status === 'ready');
  const wideRun = await publishAndStart('wide', [{
    id: 'wide',
    parallel: Array.from({ length: 6 }, (_, branch) => ({
      id: `batch${branch}`,
      parallel: Array.from({ length: branch === 5 ? 1 : 20 }, (_, index) => ({
        id: `task${branch * 20 + index}`, title: 'Task', actor: 'agent',
      })),
    })),
  }]);
  await awaitStep(`/v1/executions/${wideRun}`, ({ path, status }) => path === 'task100' && status === 'ready');
  const widePath = `${registry}/ready?group=${group}&project=${project}&limit=100`;
  const { body: wide } = await call('GET', widePath, 200, directory);
  assert.equal(wide.items.length, 100);
  assert.deepEqual([...new Set(wide.items.map(({ execution_id }) => execution_id))], [wideRun]);
  assert.deepEqual([...new Set(wide.items.map(({ agent_id }) => agent_id))], [agent.agent_id]);
  const { body: tail } = await call('GET', `${widePath}&cursor=${encodeURIComponent(wide.next_cursor)}`, 200, directory);
  assert.equal(tail.items.length, 1);
  assert.equal(tail.items[0].execution_id, wideRun);
  assert.equal(tail.next_cursor, null);
  assert.equal(new Set([...wide.items, ...tail.items].map(({ step_run_id }) => step_run_id)).size, 101);
  const otherProject = await call('GET', `${registry}/ready?group=${group}&project=${foreign.id}`, 200, directory);
  assert.deepEqual(otherProject.body.items.map(({ step_run_id }) => step_run_id), [otherReady.id]);
  const oldest = await call('GET', `${registry}/ready?group=${group}&limit=1`, 200, directory);
  assert.deepEqual(oldest.body.items.map(({ step_run_id }) => step_run_id), [otherReady.id]);
  const unknownProject = 'prj_00000000000000000000000099';
  assert.deepEqual((await call('GET', `${registry}/ready?group=${group}&project=${unknownProject}`, 200, directory)).body.items, []);
  await call('PATCH', otherRunPath, 200, { token, body: { action: 'cancel' } });
  await call('PATCH', `/v1/executions/${wideRun}`, 200, { token, body: { action: 'cancel' } });
  await call('PUT', groupProjects, 204, { token, body: { project_ids: [project] } });
  // Complete the earlier runs before starting the follow-up. Its history
  // assertions depend on the first 'checks' run being a completed run.
  await awaitRun(otherRunPath, ({ status }) => status === 'cancelled');
  await awaitRun(`/v1/executions/${wideRun}`, ({ status }) => status === 'cancelled');
  await awaitRun(execution, ({ status }) => status === 'succeeded');
  const { body: next } = await call('POST', `/v1/projects/${project}/executions`, 201, {
    token, body: { runbook: current.runbook_id, title: 'Follow-up checks' },
  });
  const nextStep = await awaitStep(`/v1/executions/${next.id}`, ({ path }) => path === 'first');
  const { body: nextClaim } = await claimStep(nextStep.id, 200);
  const previous = (action, query) => context(nextStep.id, nextClaim, action, query);
  const { body: history } = await call('GET', previous('runs'), 200, claimAuth);
  assert.deepEqual(history.items.map(({ id }) => id), [run]);
  assert.equal(history.items[0].steps, undefined);
  const { body: past } = await call('GET', previous('past-run', { run_id: run }), 200, claimAuth);
  assert.equal(past.id, run);
  await call('GET', previous('past-run', { run_id: gated }), 404, claimAuth);
  await call('GET', previous('step', { step_run_id: sr }), 404, claimAuth);
  const foreignRun = await publishAndStart('checks', [{ id: 'foreign', title: 'Foreign', actor: 'agent' }], foreign.id);
  const foreignStep = await awaitStep(`/v1/executions/${foreignRun}`, ({ path }) => path === 'foreign');
  await call('GET', context(foreignStep.id, nextClaim, 'run'), 403, claimAuth);
  await call('GET', previous('past-run', { run_id: foreignRun }), 404, claimAuth);
  await call('PUT', groupProjects, 204, { token, body: { project_ids: [project, foreign.id] } });
  await putAgent({ ...agent, project_id: foreign.id, version_id: 'v3' });
  const nextGrant = await call('POST', `/v1/step-runs/${nextStep.id}/grants`, 403, {
    ...release, body: { ...capabilityRequest, claim_id: nextClaim.id, generation: nextClaim.generation },
  });
  assert.equal(nextGrant.body.code, 'grant.not_allowed');
  await call('POST', `/v1/step-runs/${nextStep.id}/release`, 200, {
    token, body: { reason: 'Agent moved projects' },
  });
  assert.equal((await claimStep(nextStep.id, 403)).body.code, 'claim.wrong_runner');
  await putAgent({ ...agent, version_id: 'v2' });
  await call('PUT', groupProjects, 204, { token, body: { project_ids: [project] } });
  const skillPath = `${registry}/projects/${project}/skills/checks`;
  const saveSkill = async (body) => (await call('POST', `/v1/projects/${project}/skills`, 201, {
    token, body: { files: [{ path: 'SKILL.md', content: Buffer.from(body).toString('base64') }] },
  })).body;
  const originalSkill = '---\nname: checks\ndescription: Check the rollout\n---\nOriginal notes\n';
  const publishedSkill = await saveSkill(originalSkill);
  const { body: pin } = await call('GET', skillPath, 200, directory);
  assert.deepEqual(pin, { name: 'checks', hash: publishedSkill.hash, body: originalSkill });
  await call('GET', skillPath, 403, claimAuth);
  await call('GET', skillPath, 403, { token });
  await call('GET', `${registry}/projects/${foreign.id}/skills/checks`, 403, directory);
  const revisedSkill = await saveSkill(originalSkill.replace('Original', 'Updated'));
  assert.notEqual(revisedSkill.hash, pin.hash);
  assert.equal((await call('GET', skillPath, 200, directory)).body.hash, revisedSkill.hash);



  const noConnections = task('omitted');
  delete noConnections.connections;
  delete noConnections.agent.network;
  noConnections.agent.max_tokens = 750;
  noConnections.timeout = '15s';
  const policyRun = await publishAndStart('connection-policy', [{
    id: 'policies', parallel: [noConnections, { ...task('empty'), connections: [] }],
  }]);
  for (const [path, allowed, proxyStatus] of [['omitted', null, 200], ['empty', [], 403]]) {
    const policyStep = await awaitStep(`/v1/executions/${policyRun}`, (step) => step.path === path);
    const { body: binding } = await claimStep(policyStep.id, 200);
    const ask = (body, status) => call('POST', `/v1/step-runs/${policyStep.id}/grants`, status, {
      ...release, body: { ...body, claim_id: binding.id, generation: binding.generation, runner_id: runner.runner_id },
    });
    const { body: cap } = await ask({ kind: 'capability' }, 200);
    const signed = verifyAssertion(jwks, cap.grant);
    assert.deepEqual(signed.connections, allowed);
    if (path === 'omitted') {
      assert.equal(signed.kind, 'capability');
      assert.equal(signed.network, null);
      assert.equal(signed.max_tokens, 750);
      assert.equal(signed.timeout_ms, 15000);
      assert.ok(signed.step_deadline_ms > signed.iat * 1000);
      const { body } = await call('GET', `/v1/executions/${policyRun}`, 200, { token });
      assert.equal(body.steps.find(({ id }) => id === policyStep.id).runner.releases, undefined);
    }
    await ask({ ...grantBody, credential_id: 'cred_other' }, proxyStatus);
    await ask({ ...grantBody, credential_id: 'cred_model', delivery: 'model' }, 200);
  }

  const retriedRun = await publishAndStart('retry-context', [
    { ...task('retry'), retry: { max_attempts: 2 } },
  ]);
  const retryPath = `/v1/executions/${retriedRun}`;
  const retry = await awaitStep(retryPath, ({ path, status }) => path === 'retry' && status === 'ready');
  const { body: firstAttempt } = await claimStep(retry.id, 200);
  await call('POST', `/v1/step-runs/${retry.id}/logs`, 200, {
    ...claimAuth, body: { claim: firstAttempt, logs: ['Inspecting the previous rollout'] },
  });
  await call('POST', `/v1/step-runs/${retry.id}/report`, 200, {
    ...claimAuth, body: { step_run_id: retry.id, claim: firstAttempt, status: 'failed',
      summary: 'The first rollout failed', logs: ['The preflight check was inconclusive'] },
  });
  await awaitStep(retryPath, ({ id, status }) => id === retry.id && status === 'ready');
  const { body: secondAttempt } = await claimStep(retry.id, 200);
  const { body: retryContext } = await call('GET', context(retry.id, secondAttempt, 'run'), 200, claimAuth);
  assert.deepEqual(retryContext.previous_attempt, {
    attempt: 1, status: 'failed', summary: 'The first rollout failed', verdict: null,
    timed_out: false, logs: ['The preflight check was inconclusive'],
  });
  // Release gate 2026-11-21: a step the runner reported blocked goes to a
  // person. It left the runner's ready listing only in the claim check, so the
  // runner admitted it, was refused and released it every sweep.
  const blockedRun = await publishAndStart('blocked-handoff', [task('handoff')], project, { owners: ['team:oncall'] });
  const blockedPath = `/v1/executions/${blockedRun}`;
  const handoff = await awaitStep(blockedPath, ({ path, status }) => path === 'handoff' && status === 'ready');
  const { body: blockedClaim } = await claimStep(handoff.id, 200);
  await call('POST', `/v1/step-runs/${handoff.id}/report`, 200, {
    ...claimAuth, body: { step_run_id: handoff.id, claim: blockedClaim, status: 'blocked',
      summary: 'No access to the dashboard' },
  });
  await awaitStep(blockedPath, ({ id, status }) => id === handoff.id && status === 'ready');
  const { body: afterBlocked } = await call('GET', `${registry}/ready?group=${group}&project=${project}`, 200, directory);
  assert.ok(!afterBlocked.items.some(({ step_run_id: id }) => id === handoff.id), JSON.stringify(afterBlocked));
  assert.equal((await claimStep(handoff.id, 422)).body.code, 'claim.actor_mismatch');
  // The person it was handed to continues it (it was refused as
  // claim.wrong_runner, and a take-over as claim.take_over_not_allowed).
  const personClaim = { session_id: 'person', actor: { kind: 'human' } };
  const { body: handedOver } = await call('POST', `/v1/step-runs/${handoff.id}/claim`, 200, { token, body: personClaim });
  await call('POST', `/v1/step-runs/${handoff.id}/report`, 200, {
    token, body: { step_run_id: handoff.id, claim: handedOver, status: 'succeeded', summary: 'Checked by hand' },
  });
  await awaitStep(blockedPath, ({ id, status }) => id === handoff.id && status === 'succeeded');

  const cleared = await call('PATCH', projectPath, 200, { token, body: { default_agent: null } });
  assert.equal(cleared.body.default_agent ?? null, null);
}
