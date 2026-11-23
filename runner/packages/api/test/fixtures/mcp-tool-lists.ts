// Synthetic schemas based on Notion's documented tools. The lengths are from
// a live gateway discovery, with padding in place of captured provider text.
// https://developers.notion.com/guides/mcp/mcp-supported-tools
const notionToolBytes = [
  7374, 1228, 2198, 3956, 3935, 1884, 1492, 1429, 10331, 11172, 1572, 1708, 1058, 914, 2728,
  836, 794, 2199, 766, 2486, 1050, 5088, 1460, 1800, 9657, 10676, 3081, 1286, 990, 1162,
  1042, 1781, 977, 1139, 1290, 1053, 1418, 1706, 28590, 3679, 14629, 990, 982, 873, 1082,
  1622, 1084, 5491, 1040, 1041, 1145, 1014, 1135, 1474, 1176, 5084, 3356, 2733, 1733, 2781,
  1552, 889, 9228, 2411, 951, 1115, 1140, 2459, 841, 5544, 7504, 1411, 714, 1687, 1228,
  3296, 730,
];
const notionSamples = new Map<number, { name: string; description: string; inputSchema: object }>([
  [0, {
    name: 'notion-search',
    description: 'Search workspace content by query.',
    inputSchema: {
      type: 'object',
      properties: { query: { type: 'string' }, page_url: { type: 'string', format: 'uri' } },
      required: ['query'],
    },
  }],
  [3, {
    name: 'notion-fetch',
    description: 'Retrieve a page or database by URL or ID.',
    inputSchema: {
      type: 'object',
      properties: { id: { anyOf: [{ type: 'string', format: 'uri' }, { type: 'string', format: 'uuid' }] } },
      required: ['id'],
    },
  }],
  [24, {
    name: 'notion-update-data-source',
    description: 'Change the property definitions on a data source.',
    inputSchema: {
      type: 'object',
      properties: {
        data_source_id: { type: 'string', format: 'uuid' },
        properties: { type: 'object', additionalProperties: { $ref: '#/$defs/property' } },
      },
      required: ['data_source_id'],
      $defs: {
        property: {
          oneOf: [
            { type: 'object', properties: { title: { type: 'object' } }, required: ['title'] },
            { type: 'object', properties: { number: { type: 'object' } }, required: ['number'] },
          ],
        },
      },
    },
  }],
]);

export const notionTools = notionToolBytes.map((bytes, index) => {
  const tool = notionSamples.get(index) ?? {
    name: `notion-fixture-tool-${index + 1}`,
    description: 'A workspace tool with provider-supplied documentation.',
    inputSchema: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'] },
  };
  const padding = bytes - Buffer.byteLength(JSON.stringify(tool));
  return { ...tool, description: tool.description + ' '.repeat(padding) };
});

// Reduced from https://github.com/github/github-mcp-server/blob/main/pkg/github/issues.go
export const githubTools = [{
  name: 'list_issues',
  description: 'List repository issues with cursor pagination.',
  inputSchema: {
    type: 'object',
    required: ['owner', 'repo'],
    properties: {
      owner: { type: 'string' },
      repo: { type: 'string' },
      state: { enum: ['OPEN', 'CLOSED'] },
      labels: { type: 'array', items: { type: 'string' } },
      after: { type: 'string' },
    },
  },
  outputSchema: {
    type: 'object',
    properties: {
      issues: { type: 'array', items: { type: 'object', properties: { number: { type: 'integer' } } } },
      pageInfo: { type: 'object', properties: { endCursor: { type: 'string' } } },
    },
  },
}];

// Based on https://linear.app/changelog/2026-11-04-mobile-diffs
export const linearTools = [{
  name: 'list_issues',
  description: 'List issues, optionally filtering by when they were completed.',
  inputSchema: {
    type: 'object',
    properties: {
      completedAt: { type: 'string', description: 'ISO-8601 date or a relative duration such as -P7D.' },
      cursor: { type: 'string' },
    },
  },
}];
