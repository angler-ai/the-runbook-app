import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { OrgProvider } from '../context/OrgContext';
import { SkillsPage } from './SkillsPage';
import type { SkillVersionView } from './hooks';

const PRJ = 'prj_1';

const sampleSkill1: SkillVersionView = {
  name: 'gh-stack',
  description: 'Manage stacked branches and pull requests with gh-stack',
  hash: 'hash_111111111111',
  latest: true,
  reference: 'github.com/org/skills/gh-stack',
  usages: [{ runbook_id: 'rb_1', version_id: 'v1', step_ids: ['step_1'] }],
  files: [
    {
      path: 'SKILL.md',
      size: 42,
      sha256: 'sha_skill_md',
      content: btoa('# GitHub Stack\n\nInstructions on stacked PRs.'),
    },
    {
      path: 'scripts/helper.sh',
      size: 100,
      sha256: 'sha_helper',
      content: null,
    },
  ],
};

const sampleSkill2: SkillVersionView = {
  name: 'db-migrate',
  description: 'Run safe database migrations with automated rollback',
  hash: 'hash_222222222222',
  latest: true,
  reference: null,
  usages: [],
  files: [
    {
      path: 'SKILL.md',
      size: 55,
      sha256: 'sha_db_md',
      content: btoa('# Database Migrations\n\nInstructions on migrations.'),
    },
  ],
};

afterEach(() => tokenStore.clear());

function renderSkillsPage() {
  tokenStore.set('trb_pat_test');
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <OrgProvider>
        <SkillsPage projectId={PRJ} />
      </OrgProvider>
    </QueryClientProvider>,
  );
}

function baseHandlers(scopes: string[] = ['runbooks:read', 'skills:read']) {
  return [
    http.get('*/v1/me', () =>
      HttpResponse.json({
        user: { id: 'usr_1', name: 'Alice' },
        org: { id: 'org_1', title: 'Acme', role: 'admin' },
        teams: [],
        credential: {
          kind: 'personal',
          actor_kind: 'human',
          scopes,
          projects: null,
          expires_at_ms: Date.now() + 3_600_000,
        },
      }),
    ),
    http.get('*/v1/meta', () =>
      HttpResponse.json({
        limits: {
          skill_bundle_bytes: 10 * 1024 * 1024,
        },
      }),
    ),
    http.get('*/v1/orgs', () =>
      HttpResponse.json({ items: [{ id: 'org_1', title: 'Acme' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/projects', () =>
      HttpResponse.json({
        items: [{ id: PRJ, org_id: 'org_1', slug: 'prod', title: 'Production' }],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/skills', () =>
      HttpResponse.json({
        items: [sampleSkill1, sampleSkill2],
        next_cursor: null,
      }),
    ),
    http.get('*/v1/projects/:p/skills/:hash', ({ params }) => {
      if (params.hash === sampleSkill1.hash) {
        return HttpResponse.json(sampleSkill1);
      }
      return HttpResponse.json(sampleSkill2);
    }),
    http.get('*/v1/orgs/:org/users', () =>
      HttpResponse.json({ items: [{ id: 'usr_1', name: 'Alice' }], next_cursor: null }),
    ),
    http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
  ];
}

describe('SkillsPage', () => {
  it('renders list of skills and details of the active skill, passing axe check', async () => {
    server.use(...baseHandlers());

    const { container } = renderSkillsPage();

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Skills' })).toBeInTheDocument();
      expect(screen.getByText('gh-stack')).toBeInTheDocument();
      expect(screen.getByText('db-migrate')).toBeInTheDocument();
    });

    await waitFor(() => {
      expect(screen.getByText(/GitHub Stack/i)).toBeInTheDocument();
    });

    await expectNoAxeViolations(container);
  });

  it('shows scope refusal notice when token lacks skills:write, passing axe check', async () => {
    server.use(...baseHandlers(['skills:read']));

    const { container } = renderSkillsPage();

    expect(
      await screen.findByText('Sign in with email or Google to upload skills'),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /new skill/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /new version/i })).not.toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('selects another skill and renders its details', async () => {
    server.use(...baseHandlers());

    renderSkillsPage();

    await waitFor(() => {
      expect(screen.getByText('db-migrate')).toBeInTheDocument();
      expect(screen.getByText(/GitHub Stack/i)).toBeInTheDocument();
    });

    const dbOption = screen.getByRole('button', { name: /^db-migrate/i });
    fireEvent.click(dbOption);

    await waitFor(() => {
      expect(screen.getByText(/Database Migrations/i)).toBeInTheDocument();
    });
  });

  it('opens New skill dialog, validates files, uploads and displays version hash', async () => {
    let uploadedPayload: {
      files: Array<{ path: string; content: string }>;
      reference: string | null;
    } | null = null;
    const newSkillVersion: SkillVersionView = {
      name: 'cluster-scale',
      description: 'Scale Kubernetes nodes and pods safely',
      hash: 'hash_cluster_scale_99999999',
      latest: true,
      reference: 'github.com/org/skills/cluster-scale@v1.0.0',
      usages: [],
      files: [
        {
          path: 'SKILL.md',
          size: 70,
          sha256: 'sha_cs',
          content: btoa('# Cluster Scale\n\nScaling operations.'),
        },
      ],
    };

    server.use(
      ...baseHandlers(['skills:read', 'skills:write']),
      http.post('*/v1/projects/:p/skills', async ({ request }) => {
        uploadedPayload = (await request.json()) as typeof uploadedPayload;
        return HttpResponse.json(newSkillVersion, { status: 201 });
      }),
    );

    const { container } = renderSkillsPage();
    const user = userEvent.setup();

    const newSkillBtn = await screen.findByRole('button', { name: /new skill/i });
    await user.click(newSkillBtn);

    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByRole('heading', { name: 'New skill' })).toBeInTheDocument();

    // 1. Test validation failure: Missing SKILL.md
    const invalidFile = new File(['echo hello'], 'script.sh', { type: 'text/plain' });
    const fileInputs = document.querySelectorAll('input[type="file"]');
    const filesInput = fileInputs[1]!;
    fireEvent.change(filesInput, { target: { files: [invalidFile] } });

    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.getByText(/SKILL\.md is missing/i)).toBeInTheDocument();

    // 2. Test validation failure: Invalid name in frontmatter
    const badFrontmatter = `---
name: BAD_NAME!
description: Invalid uppercase and symbols
---
# Bad Skill
`;
    const badSkillFile = new File([badFrontmatter], 'SKILL.md', { type: 'text/markdown' });
    fireEvent.change(filesInput, { target: { files: [badSkillFile] } });

    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.getByText(/must be lowercase letters, digits, and hyphens/i)).toBeInTheDocument();

    // 3. Test valid upload
    const validFrontmatter = `---
name: cluster-scale
description: Scale Kubernetes nodes and pods safely
---
# Cluster Scale
Scaling operations.
`;
    const validSkillFile = new File([validFrontmatter], 'SKILL.md', { type: 'text/markdown' });
    fireEvent.change(filesInput, { target: { files: [validSkillFile] } });

    await waitFor(() => {
      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
      expect(screen.getByText('Valid bundle')).toBeInTheDocument();
      expect(screen.getByText('Scale Kubernetes nodes and pods safely')).toBeInTheDocument();
    });

    // Provide reference
    await user.type(
      within(dialog).getByLabelText(/git reference/i),
      'github.com/org/skills/cluster-scale@v1.0.0',
    );

    const uploadBtn = within(dialog).getByRole('button', { name: /upload skill/i });
    await user.click(uploadBtn);

    // Verify version hash is displayed in the success dialog
    expect(await screen.findByText('Skill uploaded successfully')).toBeInTheDocument();
    expect(screen.getByText('hash_cluster_scale_99999999')).toBeInTheDocument();
    expect(uploadedPayload).toEqual({
      files: [
        {
          path: 'SKILL.md',
          content: btoa(validFrontmatter),
        },
      ],
      reference: 'github.com/org/skills/cluster-scale@v1.0.0',
    });

    await expectNoAxeViolations(container);

    // Close dialog
    await user.click(screen.getByRole('button', { name: /done/i }));
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('opens New version dialog for selected skill with name restriction', async () => {
    let uploadedPayload: { files: Array<{ path: string; content: string }> } | null = null;
    const updatedVersion: SkillVersionView = {
      name: 'gh-stack',
      description: 'Manage stacked branches and PRs v2',
      hash: 'hash_gh_stack_v2',
      latest: true,
      reference: null,
      usages: [],
      files: [
        {
          path: 'SKILL.md',
          size: 60,
          sha256: 'sha_gh2',
          content: btoa('# GH Stack v2'),
        },
      ],
    };

    server.use(
      ...baseHandlers(['skills:read', 'skills:write']),
      http.post('*/v1/projects/:p/skills', async ({ request }) => {
        uploadedPayload = (await request.json()) as typeof uploadedPayload;
        return HttpResponse.json(updatedVersion, { status: 201 });
      }),
    );

    const { container } = renderSkillsPage();
    const user = userEvent.setup();

    const newVersionBtn = await screen.findByRole('button', { name: /new version/i });
    await user.click(newVersionBtn);

    const dialog = await screen.findByRole('dialog');
    expect(
      within(dialog).getByRole('heading', { name: 'New version for gh-stack' }),
    ).toBeInTheDocument();

    const fileInputs = document.querySelectorAll('input[type="file"]');
    const filesInput = fileInputs[1]!;

    // Mismatched skill name in frontmatter
    const wrongSkill = `---
name: other-skill
description: Different skill
---
`;
    const wrongFile = new File([wrongSkill], 'SKILL.md', { type: 'text/markdown' });
    fireEvent.change(filesInput, { target: { files: [wrongFile] } });

    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.getByText(/does not match expected skill 'gh-stack'/i)).toBeInTheDocument();

    // Matching skill name in frontmatter
    const validSkillV2 = `---
name: gh-stack
description: Manage stacked branches and PRs v2
---
# GH Stack v2
`;
    const validFile = new File([validSkillV2], 'SKILL.md', { type: 'text/markdown' });
    fireEvent.change(filesInput, { target: { files: [validFile] } });

    await waitFor(() => {
      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
      expect(screen.getByText('Valid bundle')).toBeInTheDocument();
    });

    const uploadBtn = within(dialog).getByRole('button', { name: /upload version/i });
    await user.click(uploadBtn);

    expect(await screen.findByText('Skill uploaded successfully')).toBeInTheDocument();
    expect(screen.getByText('hash_gh_stack_v2')).toBeInTheDocument();
    expect(uploadedPayload).toEqual({
      files: [
        {
          path: 'SKILL.md',
          content: btoa(validSkillV2),
        },
      ],
      reference: null,
    });

    await expectNoAxeViolations(container);
  });

  it('renders empty state when there are no skills, passing axe check', async () => {
    server.use(
      ...baseHandlers().filter((h) => !String(h.info.path).includes('/skills')),
      http.get('*/v1/projects/:p/skills', () =>
        HttpResponse.json({ items: [], next_cursor: null }),
      ),
    );

    const { container } = renderSkillsPage();

    await waitFor(() => {
      expect(screen.getByText('No skills found')).toBeInTheDocument();
    });

    await expectNoAxeViolations(container);
  });

  it('handles error state with retry', async () => {
    let callCount = 0;
    server.use(
      ...baseHandlers().filter((h) => !String(h.info.path).includes('/skills')),
      http.get('*/v1/projects/:p/skills', () => {
        callCount++;
        if (callCount === 1) {
          return new HttpResponse(null, { status: 500 });
        }
        return HttpResponse.json({ items: [sampleSkill1], next_cursor: null });
      }),
      http.get('*/v1/projects/:p/skills/:hash', () => HttpResponse.json(sampleSkill1)),
    );

    renderSkillsPage();

    await waitFor(() => {
      expect(screen.getByText('Could not load project skills')).toBeInTheDocument();
    });

    const retryBtn = screen.getByRole('button', { name: /retry/i });
    await userEvent.click(retryBtn);

    await waitFor(() => {
      expect(screen.getByText('gh-stack')).toBeInTheDocument();
    });
  });
});
