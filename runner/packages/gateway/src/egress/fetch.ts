import { Agent } from 'undici';
import { pinnedAddress } from './rules.ts';

export const pinnedFetch = async (input: Parameters<typeof fetch>[0], init?: RequestInit, dispatched?: () => void): Promise<Response> => {
  const url = new URL(input instanceof Request ? input.url : input);
  if ((url.protocol !== 'https:' && url.protocol !== 'http:') || url.username || url.password) {
    throw new Error('invalid outbound destination');
  }
  const host = url.hostname.replace(/^\[|\]$/g, '');
  const pinned = await pinnedAddress(host);
  init?.signal?.throwIfAborted();
  const dispatcher = new Agent({
    connect: {
      autoSelectFamily: false,
      lookup(hostname, options, callback) {
        if (host !== hostname) return callback(new Error('invalid outbound destination'), '', 0);
        if (options.all) return callback(null, [pinned]);
        callback(null, pinned.address, pinned.family);
      },
    },
  });
  try {
    // Node's fetch accepts the Undici dispatcher protocol. @types/node
    // describes an older Undici revision with a different FormData type.
    const options: RequestInit = { ...init, dispatcher: dispatcher as unknown as RequestInit['dispatcher'], redirect: 'error' };
    dispatched?.();
    const response = await fetch(input, options);
    // This waits for the response body to be consumed before closing sockets.
    void dispatcher.close().catch(() => undefined);
    return response;
  } catch (error) {
    await dispatcher.destroy();
    throw error;
  }
};
