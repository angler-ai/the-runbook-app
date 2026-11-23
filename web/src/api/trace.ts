let ratio: number | undefined;

export function configureBrowserTracing(value: string | undefined) {
  const parsed = value?.trim() ? Number(value) : NaN;
  ratio = Number.isFinite(parsed) && parsed >= 0 && parsed <= 1 ? parsed : undefined;
}

export function browserTraceparent(): string | undefined {
  if (ratio === undefined || !globalThis.crypto?.getRandomValues) return undefined;
  const bytes = crypto.getRandomValues(new Uint8Array(28));
  const hex = (from: number, to: number) =>
    Array.from(bytes.subarray(from, to), (byte) => byte.toString(16).padStart(2, '0')).join('');
  const trace = hex(0, 16);
  const span = hex(16, 24);
  if (/^0+$/.test(trace) || /^0+$/.test(span)) return undefined;
  const sample = new DataView(bytes.buffer, 24).getUint32(0) / 2 ** 32 < ratio;
  return `00-${trace}-${span}-${sample ? '01' : '00'}`;
}
