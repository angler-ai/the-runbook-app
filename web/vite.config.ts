import { fileURLToPath } from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig, searchForWorkspaceRoot, type ProxyOptions } from 'vite';

// Single origin, no CORS (A4): the dev server proxies the API paths to a local
// runbook-server. Override with RUNBOOK_DEV_API=http://host:port.
const apiTarget = process.env.RUNBOOK_DEV_API ?? 'http://127.0.0.1:8080';
const apiProxy: ProxyOptions = { target: apiTarget, changeOrigin: true };
const proxy = { '/v1': apiProxy, '/.well-known': apiProxy, '/api/auth': apiProxy };

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: [
      // monaco-yaml's worker (through monaco-worker-manager) imports the editor
      // worker as `monaco-editor/esm/vs/editor/editor.worker.js`; monaco-editor
      // 0.57's exports map serves that file as `monaco-editor/editor/editor.worker.js`.
      { find: /^monaco-editor\/esm\/vs\/(.+)$/, replacement: 'monaco-editor/$1' },
    ],
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy,
    fs: {
      allow: [
        searchForWorkspaceRoot(process.cwd()),
        fileURLToPath(
          new URL('../runner/packages/shared/src/credential-manifest.ts', import.meta.url),
        ),
      ],
    },
  },
  preview: { host: '127.0.0.1', port: 4173, strictPort: true, proxy },
  build: { target: 'es2023' },
});
