import { lookup } from 'node:dns/promises';
import { request } from 'node:https';
import { HttpError } from '@runbook/runner-shared/http';

const registry = /^(?=.{3,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?(?::[1-9][0-9]{0,4})?$/;
const image = /^([^/]+)\/([a-z0-9]+(?:[._-]+[a-z0-9]+)*(?:\/[a-z0-9]+(?:[._-]+[a-z0-9]+)*)*)(?::([A-Za-z0-9_][A-Za-z0-9_.-]{0,127})|@(sha256:[a-f0-9]{64}))?$/;
export const registryHost = (value: string) => registry.test(value) && Number(value.split(':')[1] ?? 443) <= 65535;
export const imageHost = (value: string) => image.exec(value)?.[1];
export const digestImage = (value: string) => Boolean(image.exec(value)?.[4]);
const accept = [
  'application/vnd.oci.image.index.v1+json', 'application/vnd.oci.image.manifest.v1+json',
  'application/vnd.docker.distribution.manifest.list.v2+json', 'application/vnd.docker.distribution.manifest.v2+json',
].join(', ');

function failed(): never {
  throw new HttpError(422, 'environment.image_unavailable', 'could not resolve the image to a digest');
}

// Use public IPv4 with TLS hostname verification and a pinned DNS result.
// Registry responses cannot redirect the request or supply arbitrary token endpoints.
async function addresses(hostname: string) {
  const found = await lookup(hostname, { family: 4, all: true });
  if (!found.length || found.some(({ address }) => {
    const [a = 0, b = 0, c = 0] = address.split('.').map(Number);
    return a === 0 || a === 10 || a === 127 || a >= 224 || (a === 100 && b >= 64 && b <= 127) ||
      (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && (b === 0 || b === 168 || (b === 88 && c === 99))) ||
      (a === 198 && (b === 18 || b === 19 || (b === 51 && c === 100))) ||
      (a === 203 && b === 0 && c === 113);
  })) failed();
  return found[0]!.address;
}

async function head(url: URL, token?: string) {
  const address = await addresses(url.hostname);
  return new Promise<{ status?: number; digest?: string; challenge?: string }>((resolve, reject) => {
    const req = request(url, {
      method: 'HEAD', signal: AbortSignal.timeout(5000), family: 4, agent: false,
      lookup: (_host, _options, callback) => callback(null, address, 4),
      headers: { accept, ...(token && { authorization: `Bearer ${token}` }) },
    }, (res) => {
      const digest = res.headers['docker-content-digest'];
      resolve({ status: res.statusCode, digest: typeof digest === 'string' ? digest : undefined,
        challenge: res.headers['www-authenticate'] });
      res.resume();
    });
    req.once('error', reject);
    req.end();
  });
}

export async function resolveImage(value: string, allowed: string[]) {
  const parsed = value.length <= 512 ? image.exec(value) : undefined;
  if (!parsed || !allowed.includes(parsed[1]!)) {
    throw new HttpError(422, 'environment.registry_not_allowed', 'use an image from an allowed registry');
  }
  const [, host, repo, tag, digest] = parsed;
  if (digest) return value;
  try {
    const target = new URL(`https://${host}/v2/${repo}/manifests/${tag ?? 'latest'}`);
    let result = await head(target);
    if (result.status === 401 && /^Bearer\s/i.test(result.challenge ?? '') &&
        (host === 'ghcr.io' || host === 'registry-1.docker.io')) {
      // These two public registries require an anonymous, pull-only token.
      const tokenURL = host === 'ghcr.io' ? new URL('https://ghcr.io/token') : new URL('https://auth.docker.io/token');
      tokenURL.searchParams.set('service', host === 'ghcr.io' ? 'ghcr.io' : 'registry.docker.io');
      tokenURL.searchParams.set('scope', `repository:${repo}:pull`);
      const response = await fetch(tokenURL, { signal: AbortSignal.timeout(5000), redirect: 'error' });
      if (!response.ok) failed();
      const { token } = await response.json() as { token?: string };
      if (!token || !/^[a-zA-Z0-9._~+/=-]{1,8192}$/.test(token)) failed();
      result = await head(target, token);
    }
    if (result.status !== 200 || !/^sha256:[a-f0-9]{64}$/.test(result.digest ?? '')) failed();
    return `${host}/${repo}@${result.digest}`;
  } catch {
    failed();
  }
}
