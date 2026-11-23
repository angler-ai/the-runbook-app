import { StringDecoder } from 'node:string_decoder';
import { clipUtf8 } from '../evidence.ts';

export const TOOL_BYTES = 30 * 1024;
export const TURN_BYTES = 96 * 1024;
const STORE_BYTES = 4 * 1024;

export function resultError(value: unknown): boolean {
  if (!value || typeof value !== 'object') return false;
  const data = value as Record<string, unknown>;
  return Boolean(data.error || data.isError || data.timed_out || (typeof data.exit_code === 'number' && data.exit_code !== 0));
}

// Render structured fields without escaping embedded code, logs, or Markdown.
function plain(value: unknown, verbatim?: string): string {
  if (Array.isArray(value)) return value.map((item) => '- ' + plain(item).replaceAll('\n', '\n  ')).join('\n') || '[]';
  if (value && typeof value === 'object') {
    return Object.entries(value).filter(([, item]) => item !== undefined).map(([key, item]) => {
      if (key === verbatim && typeof item === 'string') return key + ':\n' + item;
      const text = plain(item);
      return text.includes('\n') || (item && typeof item === 'object')
        ? key + ':\n  ' + text.replaceAll('\n', '\n  ') : key + ': ' + text;
    }).join('\n') || '{}';
  }
  return String(value ?? 'null');
}

export function renderResult(tool: string, value: unknown, source?: string): string {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return plain(value);
  const data = value as Record<string, unknown>;
  if (tool === 'bash' && typeof data.stdout === 'string') {
    return ['stdout:\n' + data.stdout, 'stderr:\n' + String(data.stderr ?? ''), 'exit code: ' + data.exit_code,
      ...(data.timed_out && typeof data.timeout_s === 'number' ? ['Timed out after ' + data.timeout_s + ' s'] : []),
      ...(data.warning ? [String(data.warning)] : []),
      ...(data.truncated ? ['[Sandbox output limit reached]'] : [])].join('\n');
  }
  if (tool === 'view' && typeof data.content === 'string') {
    const note = data.truncated ? '\n[View transport limit reached. Read a smaller range.]' :
      typeof data.next_column === 'number' ? '\n[Read the same line with start_column: ' + data.next_column + ' to continue.]' :
      typeof data.next_line === 'number' ? '\n[Read from line ' + data.next_line + ' to continue.]' : '';
    return (data.content || 'No lines in this range.') + note;
  }
  if (tool === 'grep' && typeof data.matches === 'string') return data.matches || 'No matches.';
  if (tool === 'glob' || tool === 'view') {
    const entries = data.paths ?? data.entries;
    if (Array.isArray(entries)) return (entries.join('\n') || 'No entries.') + (data.truncated ? '\n[More entries omitted]' : '');
  }
  if (source?.startsWith('MCP ') && Array.isArray(data.content)) {
    const blocks = data.content as ({ type?: string; text?: string } | null)[];
    const text = blocks.map((block) => block?.type === 'text' && typeof block.text === 'string'
      ? block.text : '[MCP ' + (block?.type ?? 'unknown') + ' content]').join('\n');
    if (text) return data.isError ? 'MCP error:\n' + text : text;
    if (data.structuredContent !== undefined) return plain(data.structuredContent);
  }
  return plain(value, tool === 'str_replace' ? 'context' : tool === 'bash_output' ? 'output' : undefined);
}

export function presentResult(body: string, maxBytes: number, reference: string, path: string | undefined, source: string | undefined, token: string, tool?: string) {
  const header = '[' + reference + ']\n' + (source ? '--- Untrusted data from ' + source.replace(/\s+/g, ' ') + ' [' + token + '] ---\n' : '');
  const footer = source ? '\n--- End untrusted data [' + token + '] ---' : '';
  const text = header + body + (path ? '\n[Saved result: ' + path + ']' : '') + footer;
  if (Buffer.byteLength(text) <= maxBytes) return text;
  const label = tool === 'view' ? '\n[Clipped. Read a smaller range from the source.]\n' : path ? '\n[Clipped. Full result: ' + path + ']\n' : '\n[Clipped]\n';
  const room = maxBytes - Buffer.byteLength(header + label + footer);
  if (room <= 0) return clipUtf8(header, maxBytes);
  const bytes = Buffer.from(body);
  const half = Math.floor(room / 2);
  let start = Math.max(0, bytes.length - half);
  while (start < bytes.length && (bytes[start]! & 0xc0) === 0x80) start++;
  return header + new StringDecoder('utf8').write(bytes.subarray(0, half)) + label + bytes.subarray(start).toString('utf8') + footer;
}

export async function storeResult(body: string, id: string, signal: AbortSignal, maxBytes: number,
  store?: (id: string, value: string, signal: AbortSignal) => Promise<string>, always = false) {
  if (store && (always || Buffer.byteLength(body) >= Math.min(STORE_BYTES, maxBytes))) {
    try { return await store(id, body, signal); }
    catch { signal.throwIfAborted(); }
  }
}
