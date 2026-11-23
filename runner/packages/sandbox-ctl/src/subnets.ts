import { isIPv4 } from 'node:net';
import { HttpError } from '@runbook/runner-shared/http';

const DEFAULT_POOL = '10.203.0.0/16';

function ipv4(value: number) {
  return [24, 16, 8, 0].map((shift) => (value >>> shift) & 255).join('.');
}

export class SandboxSubnets {
  private readonly base: number;
  private readonly count: number;
  private readonly occupied = new Set<number>();
  private next = 0;

  constructor(pool = DEFAULT_POOL) {
    const [address, prefix, extra] = pool.split('/');
    const length = Number(prefix);
    if (extra !== undefined || !address || !isIPv4(address) || !/^(?:1[2-9]|2[0-9])$/.test(prefix ?? '')) {
      throw new Error('RUNNER_SANDBOX_SUBNET_POOL must be a private IPv4 CIDR with a /12 to /29 prefix');
    }
    this.base = address.split('.').reduce((value, octet) => value * 256 + Number(octet), 0);
    const size = 2 ** (32 - length);
    const privateRanges = [[0x0a000000, 0x0affffff], [0xac100000, 0xac1fffff], [0xc0a80000, 0xc0a8ffff]];
    if (this.base % size !== 0 || !privateRanges.some(([first, last]) =>
      this.base >= first! && this.base + size - 1 <= last!)) {
      throw new Error('RUNNER_SANDBOX_SUBNET_POOL must be an aligned private IPv4 CIDR');
    }
    this.count = size / 8;
  }

  take() {
    for (let attempt = 0; attempt < this.count; attempt++) {
      const index = this.next;
      this.next = (index + 1) % this.count;
      if (this.occupied.has(index)) continue;
      this.occupied.add(index);
      const network = this.base + index * 8;
      return { index, subnet: `${ipv4(network)}/29`, gateway: ipv4(network + 1) };
    }
    throw new HttpError(503, 'sandbox.subnet_exhausted', 'sandbox subnet pool exhausted');
  }

  release(index: number) { this.occupied.delete(index); }
}
