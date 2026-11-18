# The Runbook web UI

The pilot web app: a React SPA served from the same origin as `runbook-server`.

## Setup

Requires Node 26.8.1 (`.node-version`). From `web/`:

```sh
npm ci --ignore-scripts
npm run dev        # http://127.0.0.1:5173, proxies /v1 and /.well-known
npm run check      # everything CI runs
```

The dev server proxies `/v1` and `/.well-known` to `RUNBOOK_DEV_API`
(default `http://127.0.0.1:8080`), so the browser sees a single origin and the server needs
no CORS (A4): `RUNBOOK_DEV_API=http://127.0.0.1:9090 npm run dev`.

## Scripts

| Script                    | What it does                                                                                                                                                                                                |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `dev`                     | Vite dev server with the API proxy.                                                                                                                                                                         |
| `build`                   | Production build into `dist/` (served by `runbook-server` via `RUNBOOK_WEB_DIR`, B2).                                                                                                                       |
| `check`                   | `wasm:build` (skipped without cargo), `api:check`, `lint`, `typecheck`, `test`, `test:node`, `build`, in that order.                                                                                        |
| `lint`                    | ESLint (flat config), zero warnings allowed.                                                                                                                                                                |
| `typecheck`               | `tsc -b` over the app (`tsconfig.app.json`) and the Node-side files (`tsconfig.node.json`).                                                                                                                 |
| `test`                    | Vitest (jsdom, Testing Library, MSW). Excludes `**/*.node.test.ts`.                                                                                                                                         |
| `test:node`               | `node --test` over `src/**/*.node.test.ts` (dependency-free tests, run by Node's type stripping).                                                                                                           |
| `api:gen`                 | Regenerates `src/api/schema.d.ts` from `../api/openapi.draft.json`.                                                                                                                                         |
| `api:check`               | Fails when `src/api/schema.d.ts` has drifted from the contract.                                                                                                                                             |
| `wasm:build`              | Builds `crates/runbook-web-wasm` for the browser (`web/src/wasm/runbook.wasm`, gitignored) and prints its size. Needs cargo.                                                                                |
| `e2e`                     | Builds the server and CLI (`cargo build --locked`), then Playwright against an owned database, a native server and Vite (`e2e/README.md`). Not part of `check`. Run `npx playwright install chromium` once. |
| `e2e:container`           | The same specs against the packaged image (`deploy/docker-compose.yml`): the production bundle under the server's real CSP and caching headers. Needs Docker.                                               |
| `format` / `format:check` | Prettier.                                                                                                                                                                                                   |

## Install scripts (F102)

`.npmrc` sets `ignore-scripts=true`, and CI runs `npm ci --ignore-scripts`. Nothing in the tree
needs an install script, so there is no allow-list:

- Vite 8 builds with Rolldown and Oxc, and Tailwind v4 uses Lightning CSS and Oxide. All of them ship
  their native binaries as per-platform `optionalDependencies` (for example
  `@rolldown/binding-darwin-arm64`). npm installs those without running any script. There is no
  `esbuild` in the tree.
- The only package the lockfile flags (`hasInstallScript`) is `fsevents`: optional, macOS only,
  pulled in by Vite's file watcher. It ships a prebuilt `fsevents.node`. The flagged step is a
  `node-gyp` rebuild of that same binary, so skipping it changes nothing.
- Playwright browsers are never downloaded at install time; `npx playwright install` is explicit.

If a future dependency genuinely needs its script, run it explicitly as a named step
(`npm rebuild <pkg> --ignore-scripts=false`) in CI and in this README. Don't drop the flag globally.

`overrides` pins `dompurify` (pulled in by `monaco-editor`) to a patched release, and pins
`minimatch` (under `typescript-eslint`) to a patched release. Remove each override once the parent
package ships the fix.

## Layout (A12)

| Path                                                                      | What it holds                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/ui/**`, `src/lib/**`                                                 | Presentational components: props only. Lint bans imports from `src/api`, `src/features`, `src/routes`, the router, TanStack Query, `openapi-fetch`, and `fetch`/`EventSource`/`WebSocket`. `import type` from the generated `src/api/schema` is allowed. |
| `src/ui/primitives/**`                                                    | shadcn-style primitives on Radix.                                                                                                                                                                                                                        |
| `src/ui/types.ts`                                                         | The `Outline` type: the browser compiler's outline, and the input to the Graph and Outline views. Change it with the compiler.                                                                                                                           |
| `src/styles/tokens.css`, `src/ui/theme/**`                                | Token values. `src/styles/index.css` maps them into Tailwind.                                                                                                                                                                                            |
| `src/api/**`, `src/features/**`, `src/routes/**`, `src/wasm/**`, `e2e/**` | Data, pages and routing. `src/api/schema.d.ts` is generated, never hand-edited.                                                                                                                                                                          |
| `src/gallery/**`, `src/test/**`                                           | The component gallery and test helpers.                                                                                                                                                                                                                  |

Untrusted content (instructions, summaries, outputs, logs, checklist text, prompts) renders as
text only (A10). Lint bans `dangerouslySetInnerHTML` and explicit `any`.

## Styling

Tailwind v4 is configured entirely in `src/styles/index.css`. Its `@theme inline` block maps
utilities to the `--rb-*` variables from `tokens.css`:

- Surfaces and text: `bg-canvas`, `bg-surface`, `bg-surface-raised`, `text-fg`, `text-fg-muted`.
- Borders: `border-border`, `border-border-input`.
- Accent and danger: `bg-accent`, `text-accent-fg`, `hover:bg-accent-hover`, `bg-danger`.
- Status, verdict and markers: `bg-status-<status>-bg`, `text-status-<status>-fg`,
  `text-verdict-<v>-fg`, `border-verdict-<v>-border`, `border-stale-border`, `border-amended-border`.
- Type: `font-sans`, `font-mono`, and `text-2xs` through `text-xl` (11, 12, 13, 14, 16, 20 and 24px).
- Radii: `rounded-sm`, `rounded-md`, `rounded-lg`.

Tailwind's default color palette is removed on purpose, so a utility like `bg-blue-500` generates
nothing; use the tokens. Themes switch on `data-theme="light|dark"`. With no attribute set, they
follow `prefers-color-scheme`. Tokens already switch with the theme, so `dark:` overrides should be
rare; when used, `dark:` follows an explicit `data-theme="dark"` ancestor.

Fonts are self-hosted from `@fontsource` (Inter 400/500/600, JetBrains Mono 400) and imported in
`src/main.tsx`. There are no CDNs.

## Component gallery (`/__gallery`, dev only)

Add a `*.gallery.tsx` file next to your components:

```tsx
import { defineGallery } from '../../gallery/registry';
import { StatusPill } from './StatusPill';

export default defineGallery({
  id: 'status-pill', // unique, URL-safe
  group: 'Status',
  title: 'StatusPill',
  examples: [{ name: 'Every status', render: () => <StatusPill status="running" /> }],
});
```

The gallery finds these files through `import.meta.glob`, so no central list needs editing. It
renders each example twice, in a light pane and a dark pane, so examples must not hard-code DOM
ids; use `useId()` inside a small demo component. Portaled content (dialogs, menus, toasts) follows
the page theme, not the pane. `/__gallery?entry=<id>` shows a single entry, for screenshots. The
route and its chunk are excluded from production builds.

## Tests

- **Vitest and Testing Library** on jsdom. `src/test/setup.ts` loads the jest-dom matchers, starts
  MSW and fills a few jsdom gaps that Radix needs.
- **axe:** end component tests with `await expectNoAxeViolations(container)` (from
  `src/test/axe.ts`). It runs the WCAG 2.x A/AA rules. Color contrast is off under jsdom; it is
  covered by `scripts/check-contrast.mjs` (A1) and by `@axe-core/playwright` in e2e, which fails on
  moderate, serious and critical findings in both themes (`e2e/README.md`).
- **MSW:** `src/test/msw/fixtures.ts` imports every `../api/examples/*.json`, typed against the
  generated schema. `handlers.ts` serves them for the implemented routes. Unhandled requests fail
  the test, and tests override handlers with `server.use(...)`. Note that `openapi-fetch` captures
  `fetch` when the client is created, so pass `fetch: (r) => globalThis.fetch(r)`. Otherwise MSW
  (and any later fetch wrapper) is bypassed.
- **`*.node.test.ts`** run with `node --test` only (Vitest excludes them). Import siblings with
  the `.ts` extension (for example `import { x } from './sse.ts'`).

## API client

`openapi-typescript` generates `src/api/schema.d.ts`. `openapi-fetch` is the runtime client;
its middleware (bearer auth, idempotency keys, `If-Match` passthrough, problem+json) is in
`src/api/client.ts`. The draft contract may still break during the pilot (F94): run
`npm run api:gen` after every contract change and commit the result. `api:check` (and therefore
`check`) fails until you do.

## Browser compiler (`src/wasm/`)

`npm run wasm:build` builds `crates/runbook-web-wasm` into `src/wasm/runbook.wasm` (gitignored);
`check` builds it when cargo is available, and its Vitest suite skips (with a warning) without it.
`worker.ts` runs it off the main thread and `client.ts` is the page's promise API. Results are
provisional: the server's diagnostics replace them on save and publish (F99). Limits, all checked
before the request reaches the module (wasm memory never shrinks):

- a source over 1 MiB (UTF-8) is refused with `spec.source_limit`, the server's 413;
- a whole request over 8 MiB (source plus directory context) is refused with `abi.input_limit`;
- a call that runs 10 s after reaching the head of the worker's queue rejects with
  `wasm.timeout`; the worker is terminated and the calls behind it rerun on a fresh one.

## Production serving

`runbook-server` serves `dist/` from `RUNBOOK_WEB_DIR` on the API's origin (B2, A4: no CORS):
hashed `assets/*` are immutable for a year, `index.html` is `no-store`, other files are `no-cache`
and revalidate through their strong `ETag`/`Last-Modified` (304). Every file carries the CSP in
`crates/runbook-server/src/web_assets.rs` (`script-src 'self' 'wasm-unsafe-eval'`, `worker-src
'self' blob:`, `connect-src 'self'`, `font-src 'self' data:`, inline styles allowed). The Vite dev
server sends no CSP, so only `npm run e2e:container` tests the app under it; a CSP refusal fails
that run.
