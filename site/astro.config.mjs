// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

// The open-source documentation for The Runbook (DOCS1). Hosting at
// docs.therunbook.app waits on the DNS record; until then `site` is only
// used for canonical URLs and the sitemap.
export default defineConfig({
  site: 'https://docs.therunbook.app',
  integrations: [
    starlight({
      title: 'The Runbook',
      description:
        'Runbooks that people and agents run together: documentation for the open-source server, CLI, MCP server and API.',
      logo: { src: './src/assets/logo.svg', alt: '' },
      favicon: '/favicon.svg',
      head: [
        { tag: 'meta', attrs: { name: 'theme-color', content: '#4338ca' } },
      ],
      customCss: [
        '@fontsource/dm-sans/400.css',
        '@fontsource/dm-sans/500.css',
        '@fontsource/dm-sans/600.css',
        '@fontsource/ibm-plex-mono/400.css',
        '@fontsource/ibm-plex-mono/500.css',
        './src/styles/theme.css',
      ],
      components: {
        Hero: './src/components/Hero.astro',
      },
      expressiveCode: {
        themes: ['github-dark-default', 'github-light'],
        styleOverrides: {
          borderRadius: '10px',
          codeFontFamily: "'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, monospace",
          uiFontFamily: "'DM Sans', -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
          codeFontSize: '0.84rem',
        },
      },
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/angler-ai/the-runbook-app',
        },
      ],
      editLink: {
        baseUrl: 'https://github.com/angler-ai/the-runbook-app/edit/main/site/',
      },
      lastUpdated: false,
      plugins: [
        // Every internal link and anchor must resolve. /api/ is a page of
        // its own (Scalar), outside the docs collection.
        starlightLinksValidator({ exclude: ['/api/', '/api/openapi.json'], errorOnInvalidHashes: true }),
      ],
      sidebar: [
        { label: 'Getting started', items: [{ autogenerate: { directory: 'getting-started' } }] },
        { label: 'Concepts', items: [{ autogenerate: { directory: 'concepts' } }] },
        {
          label: 'The runbook language',
          items: [
            { autogenerate: { directory: 'language' } },
            { label: 'Key reference', link: '/reference/keys/' },
            { label: 'Diagnostics', link: '/reference/diagnostics/' },
          ],
        },
        { label: 'Samples', collapsed: true, items: [{ autogenerate: { directory: 'samples' } }] },
        { label: 'Agents', items: [{ autogenerate: { directory: 'agents' } }] },
        {
          label: 'Webhooks',
          items: [
            { autogenerate: { directory: 'webhooks' } },
            { label: 'Event reference', link: '/reference/webhook-events/' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'CLI', link: '/reference/cli/' },
            { label: 'MCP server', link: '/reference/mcp/' },
            { label: 'API explorer', link: '/api/', attrs: { target: '_self' } },
            { label: 'Environment variables', link: '/reference/environment/' },
          ],
        },
        { label: 'Operating it', items: [{ autogenerate: { directory: 'operating' } }] },
        { label: 'Security', items: [{ autogenerate: { directory: 'security' } }] },
        { label: 'Changelog', items: [{ autogenerate: { directory: 'changelog' } }] },
      ],
    }),
  ],
});
