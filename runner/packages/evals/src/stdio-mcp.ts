import { dirname, join } from 'node:path';
import { readFile } from 'node:fs/promises';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { fixtureServer } from './mock-mcp.ts';
import type { Task } from './tasks.ts';

const path = process.argv[2];
if (!path) throw new Error('Expected fixture MCP data path');
const tools: Task['mcp'] = JSON.parse(await readFile(path, 'utf8'));
await fixtureServer(tools, (name) => readFile(join(dirname(path), 'workspace', name), 'utf8').catch(() => undefined)).connect(new StdioServerTransport());
