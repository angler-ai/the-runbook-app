import { expect, it, vi } from 'vitest';
import { observeProviderFetch } from '../src/model/stream-fetch.ts';

it.each(['unbroken line', 'multiline event', 'total response', 'error response'])('cancels an oversized %s', async (kind) => {
  const total = kind.endsWith('response');
  const chunk = kind === 'unbroken line' ? 'x'.repeat(64 * 1024)
    : 'data: ' + 'x'.repeat(64 * 1024) + (kind === 'multiline event' ? '\n' : '\n\n');
  const bytes = new TextEncoder().encode(chunk);
  const cancel = vi.fn();
  let sent = 0;
  const fetcher = observeProviderFetch(async () => new Response(new ReadableStream({
    pull(controller) {
      if (sent++ < (total ? 300 : 20)) controller.enqueue(bytes);
      else controller.close();
    },
    cancel,
  }), { status: kind === 'error response' ? 500 : 200 }), () => {}, () => {});
  const response = await fetcher('https://fixture.example');
  const reader = response.body!.getReader();
  let received = 0;
  await expect((async () => {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      received += value.byteLength;
    }
  })()).rejects.toThrow('provider stream exceeds');
  expect(received).toBeLessThanOrEqual(total ? 16 * 1024 ** 2 : 1024 ** 2);
  await vi.waitFor(() => expect(cancel).toHaveBeenCalledOnce());
});

it('uses the profile byte cap before forwarding streaming data', async () => {
  const fetcher = observeProviderFetch(async () => new Response('data: {}\n\n'), () => {}, () => {}, undefined, 8);
  const response = await fetcher('https://fixture.example');
  await expect(response.text()).rejects.toThrow('provider stream exceeds response limit');
});
