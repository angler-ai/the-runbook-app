// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

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
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/angler-ai/the-runbook-app',
        },
      ],
      lastUpdated: false,
      sidebar: [
        { label: 'Getting started', items: [{ autogenerate: { directory: 'getting-started' } }] },
        { label: 'Concepts', items: [{ autogenerate: { directory: 'concepts' } }] },
        { label: 'The runbook language', items: [{ autogenerate: { directory: 'language' } }] },
        { label: 'Samples', items: [{ autogenerate: { directory: 'samples' } }] },
        { label: 'Agents', items: [{ autogenerate: { directory: 'agents' } }] },
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
