// A fake core server for runner tests (the real R0a routes are built in
// parallel): `POST /v1/step-runs/{sr}/grants`, `POST …/grants/revoked` and
// `GET /.well-known/jwks.json`, per the plan's Shared contracts. Grants are
// JWTs (typ rb-grant+jwt, EdDSA) with exp = the lease deadline. Tests set
// up leases, take steps over and swap in a rogue signing key.
import { randomBytes } from 'node:crypto';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { type GrantClaims, type GrantRequest, JWT_TYP_GRANT } from '../src/contracts/core.ts';
import { generateSigningKey, type SigningKey, signJwt } from './jwt.ts';

export interface FakeLease {
  claim_id: string;
  generation: number;
  runner_id: string;
  group_id: string;
  agent_id: string;
  agent_version: string;
  project_id: string;
  /** Unix seconds. */
  deadline: number;
  /** Credentials the step may use (its `connections` narrowing within the ceiling). */
  credentials: string[] | null;
  model_profile?: string | null;
  max_tokens?: number | null;
  timeout_ms?: number | null;
  step_deadline_ms?: number | null;
  destructive?: boolean;
  reach?: 'read';
  network?: string[] | null;
  /** Set by takeOver: the claim moved on. */
  moved?: boolean;
}

export interface RecordedRequest {
  method: string;
  path: string;
  token: string;
  body: unknown;
}

export class FakeCore {
  url = '';
  readonly releaseToken: string;
  readonly key: SigningKey = generateSigningKey('grant-1');
  readonly leases = new Map<string, FakeLease>();
  readonly requests: RecordedRequest[] = [];
  readonly revoked: { step_run_id: string; credential_id: string; reason: string }[] = [];
  /** Sign with a key that is not in the JWKS. */
  rogue = false;
  /** Override the audience of issued grants. */
  audienceOverride: string | undefined;
  /** Answer the next grant requests with this error. */
  failNext: { status: number; code: string } | undefined;
  private readonly rogueKey = generateSigningKey('grant-1');
  private readonly server: http.Server;
  private readonly now: () => number;

  constructor(options: { now?: () => number; releaseToken?: string } = {}) {
    this.now = options.now ?? Date.now;
    this.releaseToken = options.releaseToken ?? 'fake-core-release-token-0000000000000000';
    this.server = http.createServer((req, res) => void this.handle(req, res));
  }

  async start(): Promise<this> {
    await new Promise<void>((resolve) => this.server.listen(0, '127.0.0.1', resolve));
    this.url = `http://127.0.0.1:${(this.server.address() as AddressInfo).port}`;
    return this;
  }

  async close(): Promise<void> {
    this.server.closeAllConnections();
    await new Promise<void>((resolve) => this.server.close(() => resolve()));
  }

  /** Add a live lease for a step run; returns its claim. */
  lease(stepRunId: string, overrides: Partial<FakeLease> = {}): FakeLease {
    const lease: FakeLease = {
      claim_id: `cl_${randomBytes(4).toString('hex')}`,
      generation: 1,
      runner_id: 'rn_one',
      group_id: 'rg_default',
      agent_id: 'ag_checker',
      agent_version: 'av_1',
      project_id: 'pr_ops',
      deadline: Math.floor(this.now() / 1000) + 600,
      credentials: [],
      ...overrides,
    };
    this.leases.set(stepRunId, lease);
    return lease;
  }

  /** A person takes the step over: grants for the old claim are refused. */
  takeOver(stepRunId: string): void {
    const lease = this.leases.get(stepRunId);
    if (lease) lease.moved = true;
  }

  grantRequests(kind?: GrantRequest['kind']): RecordedRequest[] {
    return this.requests.filter(
      (r) => r.path.endsWith('/grants') && (kind === undefined || (r.body as GrantRequest).kind === kind),
    );
  }

  sign(claims: GrantClaims): string {
    return signJwt(this.rogue ? this.rogueKey : this.key, JWT_TYP_GRANT, claims);
  }

  private async handle(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    const text = Buffer.concat(chunks).toString('utf8');
    const body: unknown = text ? JSON.parse(text) : undefined;
    const path = new URL(req.url ?? '/', 'http://x').pathname;
    const token = (req.headers.authorization ?? '').replace(/^Bearer /, '');
    this.requests.push({ method: req.method ?? '', path, token, body });
    const send = (status: number, payload?: unknown) => {
      res.writeHead(status, { 'content-type': 'application/json' });
      res.end(payload === undefined ? '' : JSON.stringify(payload));
    };
    const problem = (status: number, code: string) => send(status, { status, code, title: code });

    if (req.method === 'GET' && path === '/.well-known/jwks.json') return send(200, { keys: [this.key.jwk] });
    if (req.method === 'GET' && path === '/v1/meta') return send(200, { runner_contract_version: 3 });
    const grant = /^\/v1\/step-runs\/([^/]+)\/grants$/.exec(path);
    const revoked = /^\/v1\/step-runs\/([^/]+)\/grants\/revoked$/.exec(path);
    if ((grant || revoked) && token !== this.releaseToken) return problem(401, 'auth.invalid_token');
    if (req.method === 'POST' && revoked) {
      const b = body as { credential_id: string; reason: string };
      this.revoked.push({ step_run_id: decodeURIComponent(revoked[1] ?? ''), credential_id: b.credential_id, reason: b.reason });
      return send(204);
    }
    if (req.method === 'POST' && grant) {
      if (this.failNext) return problem(this.failNext.status, this.failNext.code);
      const sr = decodeURIComponent(grant[1] ?? '');
      const r = body as GrantRequest;
      const lease = this.leases.get(sr);
      const now = Math.floor(this.now() / 1000);
      if (!lease || lease.moved || lease.claim_id !== r.claim_id || lease.generation !== r.generation) {
        return problem(409, 'grant.claim_moved');
      }
      if (lease.deadline <= now) return problem(409, 'grant.lease_expired');
      if (lease.runner_id !== r.runner_id) return problem(403, 'grant.not_allowed');
      if (r.kind === 'credential' && (!r.credential_id || (r.delivery !== 'model' && lease.credentials !== null && !lease.credentials.includes(r.credential_id)))) {
        return problem(403, 'grant.not_allowed');
      }
      const claims: GrantClaims = {
        iss: this.url,
        aud: (this.audienceOverride ?? `runner:${lease.runner_id}`) as GrantClaims['aud'],
        sub: sr,
        kind: r.kind,
        claim_id: lease.claim_id,
        generation: lease.generation,
        runner_id: lease.runner_id,
        group_id: lease.group_id,
        agent_id: lease.agent_id,
        agent_version: lease.agent_version,
        project_id: lease.project_id,
        destructive: lease.destructive ?? false,
        ...(lease.reach && { reach: lease.reach }),
        network: lease.network ?? null,
        connections: lease.credentials,
        model_profile: lease.model_profile ?? null,
        max_tokens: lease.max_tokens ?? null,
        timeout_ms: lease.timeout_ms ?? null,
        step_deadline_ms: lease.step_deadline_ms ?? null,
        ...(r.kind === 'credential'
          ? { credential_id: r.credential_id, credential_version: r.credential_version, delivery: r.delivery }
          : {}),
        iat: now,
        exp: lease.deadline,
        jti: `gr_${randomBytes(6).toString('hex')}`,
      };
      return send(200, { grant: this.sign(claims), expires_at: lease.deadline * 1000 });
    }
    return problem(404, 'route.not_found');
  }
}
