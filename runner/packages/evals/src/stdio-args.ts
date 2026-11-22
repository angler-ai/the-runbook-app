import { fileURLToPath } from 'node:url';

/**
 * Node arguments that start the stdio fixture MCP server. Run from TypeScript sources
 * (tests and the eval CLI), the child must resolve workspace packages to their sources
 * through the `development` condition, as Vitest does; from a build it uses `dist`.
 */
export function stdioMcpArgs(): string[] {
  const fromSource = import.meta.url.endsWith('.ts');
  const script = fileURLToPath(new URL(fromSource ? './stdio-mcp.ts' : './stdio-mcp.js', import.meta.url));
  return fromSource ? ['--conditions=development', script] : [script];
}
