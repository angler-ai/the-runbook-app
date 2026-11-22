import { lookup } from 'node:dns/promises';
import { BlockList, isIP } from 'node:net';
import { HttpError } from '@runbook/runner-shared/http';

export type HostAccess = 'read' | 'write';
export type HostRules = Readonly<Record<string, HostAccess>>;
const blocked = new BlockList();
for (const [network, prefix] of [
  ['0.0.0.0', 8], ['10.0.0.0', 8], ['100.64.0.0', 10], ['127.0.0.0', 8],
  ['169.254.0.0', 16], ['172.16.0.0', 12], ['192.0.0.0', 24], ['192.0.2.0', 24],
  ['192.168.0.0', 16], ['198.18.0.0', 15], ['198.51.100.0', 24], ['203.0.113.0', 24],
  ['224.0.0.0', 3], ['168.63.129.16', 32],
] as const) blocked.addSubnet(network, prefix, 'ipv4');
const globalV6 = new BlockList();
globalV6.addSubnet('2000::', 3, 'ipv6');
for (const [network, prefix] of [['2001::', 23], ['2001:db8::', 32], ['2002::', 16], ['3fff::', 20]] as const) {
  blocked.addSubnet(network, prefix, 'ipv6');
}

export function deny(): never {
  throw new HttpError(403, 'egress.denied', 'egress denied');
}

export function policyForHost(host: string, layers: readonly HostRules[], boundHosts: readonly string[]): HostAccess {
  const values = layers.map((layer) => Object.hasOwn(layer, host) ? layer[host] : undefined);
  if (layers.length !== 3 || values.some((value) => value !== 'read' && value !== 'write') || !boundHosts.includes(host)) deny();
  return values.includes('read') ? 'read' : 'write';
}

export async function pinnedAddress(
  host: string,
  resolve: (hostname: string) => Promise<{ address: string; family: number }[]> = (hostname) => lookup(hostname, { all: true }),
) {
  let addresses: Awaited<ReturnType<typeof resolve>>;
  try {
    addresses = isIP(host) ? [{ address: host, family: isIP(host) }] : await resolve(host);
  } catch {
    return deny();
  }
  for (const { address, family } of addresses) {
    if (isIP(address) !== family) deny();
    if (family === 4 ? blocked.check(address, 'ipv4') : !globalV6.check(address, 'ipv6') || blocked.check(address, 'ipv6')) deny();
  }
  return addresses[0] ?? deny();
}

export function connectHost(authority: string): string {
  const match = /^([a-z0-9](?:[a-z0-9.-]*[a-z0-9])?)(?::443)?$/i.exec(authority);
  const host = match?.[1]?.toLowerCase();
  if (!host || isIP(host) || host.length > 253) deny();
  return host;
}

export function checkAuthority(host: string, servername: string | undefined, header: string | undefined): void {
  if (!servername || !header || servername.toLowerCase() !== host || connectHost(header) !== host) deny();
}

export function checkMethod(access: HostAccess, method: string): void {
  if (method === 'CONNECT' || method === 'TRACE' || (access === 'read' && !['GET', 'HEAD', 'OPTIONS'].includes(method))) deny();
}
