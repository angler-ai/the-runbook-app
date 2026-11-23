import { OAuth2Client } from 'google-auth-library';
import { expect, it, vi } from 'vitest';
import { VertexIdentity, vertexTargets } from '../src/vertex-identity.ts';

const account = 'runbook-vertex@angler-dev.iam.gserviceaccount.com';
const now = () => new Date(1000);
const response = (accessToken: string, expiry: number) => Object.assign(new Response(), {
  config: { url: new URL('https://iamcredentials.googleapis.com'), headers: new Headers() },
  data: { accessToken, expireTime: new Date(expiry).toISOString() },
});

function fixture() {
  const client = new OAuth2Client();
  const request = vi.fn<() => Promise<ReturnType<typeof response>>>();
  client.request = request as typeof client.request;
  const identity = new VertexIdentity({ auth: { getClient: async () => client }, now });
  return { identity, request };
}

it('pairs each mint with its own expiry when IAM responses arrive out of order', async () => {
  const { identity, request } = fixture();
  let finishFirst!: (value: ReturnType<typeof response>) => void;
  const first = new Promise<ReturnType<typeof response>>((resolve) => { finishFirst = resolve; });
  request.mockReturnValueOnce(first).mockResolvedValueOnce(response('fake-renewed-token', 9000));

  const slow = identity.access(account, 5000);
  const fast = identity.access(account, 5000);
  expect(await fast).toEqual({ material: { fields: { access_token: 'fake-renewed-token' } }, expiresAt: 5000 });
  finishFirst(response('fake-initial-token', 3000));
  expect(await slow).toEqual({ material: { fields: { access_token: 'fake-initial-token' } }, expiresAt: 3000 });
});

it.each([
  [Object.assign(new Error('fake-sensitive-detail'), { response: { status: 403 } }), 'vertex.adc_forbidden'],
  [Object.assign(new Error('fake-sensitive-detail'), { response: { status: 503 } }), 'vertex.adc_retry'],
  [new Error('fake-sensitive-detail'), 'vertex.adc_unavailable'],
])('classifies Google failures for %j without retaining upstream details', async (error, code) => {
  const { identity, request } = fixture();
  request.mockRejectedValue(error);
  const access = identity.access(account, 5000);
  await expect(access).rejects.toMatchObject({ code });
  await expect(access).rejects.not.toThrow('fake-sensitive-detail');
});

it.each([
  ['fake-expired-token', 1000], ['fake token', 3000], ['fake-overlong-token', 3_601_000],
])('refuses unusable IAM material', async (token, expiry) => {
  const { identity, request } = fixture();
  request.mockResolvedValue(response(token, expiry));
  await expect(identity.access(account, 5000)).rejects.toMatchObject({ code: 'vertex.adc_unavailable' });
});

it('disables attached Vertex with an absent or empty installer allowlist', () => {
  for (const raw of [undefined, '[]']) {
    const identity = new VertexIdentity({ targets: vertexTargets(raw) });
    expect(() => identity.assertTarget('vertex-gemini', 'angler-dev', 'global')).toThrow();
  }
});

it('rejects malformed installer policies without reporting their contents', () => {
  const valid = { project: 'angler-dev', locations: ['global'], service_account: account };
  expect(vertexTargets(JSON.stringify([valid]))).toEqual([valid]);
  for (const raw of ['{', JSON.stringify([valid, valid]), JSON.stringify([{ ...valid, locations: [] }]),
    JSON.stringify([{ ...valid, locations: ['global', '../invalid'] }]), JSON.stringify([{ ...valid, service_account: '&invalid' }])]) {
    expect(() => vertexTargets(raw)).toThrow('RUNNER_VERTEX_TARGETS must be a JSON array');
  }
});
