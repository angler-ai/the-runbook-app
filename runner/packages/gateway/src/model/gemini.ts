import { createVertex } from '@ai-sdk/google-vertex';
import type { LanguageModelV4, LanguageModelV4CallOptions } from '@ai-sdk/provider';

export function geminiEndpoint(endpoint: URL) {
  const match = /^\/v1\/projects\/([\w-]+)\/locations\/([\w-]+)\/publishers\/google\/models\/?$/.exec(endpoint.pathname);
  const location = match?.[2];
  const host = location === 'global' ? 'aiplatform.googleapis.com' :
    location === 'us' || location === 'eu' ? `aiplatform.${location}.rep.googleapis.com` : `${location}-aiplatform.googleapis.com`;
  if (!match || endpoint.hostname !== host || endpoint.port) throw new Error('invalid Gemini Vertex endpoint');
  return match[1]!;
}

export function geminiModel(modelId: string, endpoint: URL, token: string, fetcher: typeof fetch, useQuotaProjectHeader: boolean): LanguageModelV4 {
  const project = geminiEndpoint(endpoint);
  if (!/^gemini-[\w-]+$/.test(modelId)) throw new Error('invalid Gemini Vertex model');
  // The SDK's Express-mode switch bypasses its ADC lookup. The fetch adapter
  // removes the placeholder header before adding the released OAuth token.
  const provider = (call: LanguageModelV4CallOptions) => createVertex({
    apiKey: () => 'unused',
    baseURL: new URL('..', endpoint.href.replace(/\/?$/, '/')).href,
    fetch: (input, init) => {
      const headers = new Headers(init?.headers);
      headers.delete('x-goog-api-key');
      headers.set('authorization', `Bearer ${token}`);
      // Attached inference uses the project in the resource path. Sending
      // an explicit quota-project override also requires serviceusage.services.use.
      if (useQuotaProjectHeader) headers.set('x-goog-user-project', project);
      // The installed Vertex SDK omits IDs on requests even when the model
      // returns them. Restore them in the same order as the converted parts.
      const ids = call.prompt.flatMap((message) => typeof message.content === 'string' ? [] :
        message.content.flatMap((part) => part.type === 'tool-call' || part.type === 'tool-result' ? [part.toolCallId] : []));
      const body = JSON.parse(String(init?.body)) as {
        continuationToken?: string;
        generationConfig?: { thinkingConfig?: { thinkingLevel?: string } };
        contents: { parts: { functionCall?: { id?: string }; functionResponse?: { id?: string } }[] }[];
      };
      const thinking = body.generationConfig?.thinkingConfig;
      if (thinking?.thinkingLevel) thinking.thinkingLevel = thinking.thinkingLevel.toUpperCase();
      if (body.continuationToken) throw new Error('Gemini long decode is outside the turn budget');
      let index = 0;
      for (const { parts } of body.contents) {
        for (const part of parts) {
          const tool = part.functionCall ?? part.functionResponse;
          if (tool) tool.id = ids[index++];
        }
      }
      if (index !== ids.length) throw new Error('Gemini function call conversion lost an ID');
      return fetcher(input, { ...init, headers, body: JSON.stringify(body) });
    },
  })(modelId);
  return { specificationVersion: 'v4', provider: 'google.vertex.chat', modelId, supportedUrls: {},
    doGenerate: (call) => provider(call).doGenerate(call),
    doStream: (call) => provider(call).doStream(call),
  };
}
