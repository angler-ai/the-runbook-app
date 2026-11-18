import { Link, useParams } from '@tanstack/react-router';
import { useQueries } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { Plus } from 'lucide-react';
import { ApiError } from '../../api/client';
import { api, type Schemas } from '../../api/client';
import { useMe, useRunbooks } from '../../api/hooks';
import { queryKeys } from '../../api/query-keys';
import { ErrorState, ListPaneHeading, PageHeader, Skeleton, SplitPane } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import {
  SkillDetail,
  type SkillHistoryItem,
  type SkillUsageLabel,
} from '../../ui/skills/SkillDetail';
import { SkillList } from '../../ui/skills/SkillList';
import { useOrgContext } from '../context/OrgContext';
import { decodeBase64Utf8, useProjectSkills, useSkillDetail } from './hooks';
import { UploadSkillDialog } from './UploadSkillDialog';

export function SkillsPage({ projectId: propProjectId }: { projectId?: string } = {}) {
  let routeProject: string | undefined;
  try {
    const params = useParams({ strict: false }) as { project?: string };
    routeProject = params.project;
  } catch {
    // In tests or outside router
  }
  const { projects } = useOrgContext();
  const currentProject = projects.find(
    (p) => p.slug === (propProjectId ?? routeProject) || p.id === (propProjectId ?? routeProject),
  );
  const projectId = propProjectId ?? currentProject?.id ?? routeProject;

  const me = useMe();
  const canUpload = Boolean(me.data?.credential?.scopes?.includes('skills:write'));

  const skillsQuery = useProjectSkills(projectId);
  const [selectedName, setSelectedName] = useState<string | null>(null);

  // Dialog state
  const [uploadOpen, setUploadOpen] = useState(false);
  const [uploadExpectedName, setUploadExpectedName] = useState<string | undefined>(undefined);

  const rawSkills = skillsQuery.data?.items;
  const skills = useMemo(() => rawSkills ?? [], [rawSkills]);

  // Determine active skill: selectedName or first skill
  const activeSkillBrief = useMemo(() => {
    if (skills.length === 0) return null;
    if (selectedName) {
      return skills.find((s) => s.name === selectedName) ?? skills[0];
    }
    return skills[0];
  }, [skills, selectedName]);

  const detailQuery = useSkillDetail(projectId, activeSkillBrief?.hash);

  // Fallback to activeSkillBrief while detail is loading
  const activeSkill = detailQuery.data ?? activeSkillBrief;

  // Compute history for selected skill
  const history: SkillHistoryItem[] = useMemo(() => {
    if (!activeSkill) return [];
    return skills
      .filter((s) => s.name === activeSkill.name)
      .map((s) => ({
        version: s.hash.slice(0, 8),
        hash: s.hash,
        latest: s.latest,
      }));
  }, [skills, activeSkill]);

  // Name each usage: the runbook's title, linked, and its version number.
  const runbooks = useRunbooks(projectId);
  const usedRunbookIds = useMemo(
    () => [...new Set((activeSkill?.usages ?? []).map((u) => u.runbook_id))],
    [activeSkill],
  );
  const versionPages = useQueries({
    queries: usedRunbookIds.map((rb) => ({
      queryKey: [...queryKeys.runbookVersions(rb), 'skill-usage'],
      queryFn: async () => {
        const { data } = await api.GET('/v1/runbooks/{rb}/versions', {
          params: { path: { rb }, query: { limit: 100 } },
        });
        return (data as { items: Schemas['VersionView'][] }).items;
      },
    })),
  });
  const versionNumbers = useMemo(() => {
    const out = new Map<string, number>();
    for (const page of versionPages) {
      for (const v of page.data ?? []) {
        if (v.runbook_version != null) out.set(v.runbook_version_id, v.runbook_version);
      }
    }
    return out;
  }, [versionPages]);
  const projectRef = currentProject?.slug ?? projectId;
  const describeUsage = (usage: { runbook_id: string; version_id: string }): SkillUsageLabel => {
    const runbook = runbooks.data?.find((r) => r.runbook_id === usage.runbook_id);
    const number = versionNumbers.get(usage.version_id);
    return {
      runbook:
        runbook && projectRef ? (
          <Link
            to="/p/$project/runbooks/$rb"
            params={{ project: projectRef, rb: runbook.runbook_id }}
            className="text-accent hover:underline"
          >
            {runbook.title || runbook.slug}
          </Link>
        ) : (
          (runbook?.title ?? runbook?.slug ?? usage.runbook_id)
        ),
      version: number != null ? `v${number}` : undefined,
    };
  };

  // Extract SKILL.md markdown
  const skillMarkdown = useMemo(() => {
    if (!activeSkill?.files) return undefined;
    const file = activeSkill.files.find(
      (f) => f.path.toLowerCase() === 'skill.md' || f.path.endsWith('/SKILL.md'),
    );
    if (!file?.content) return undefined;
    return decodeBase64Utf8(file.content);
  }, [activeSkill]);

  const handleOpenNewSkill = () => {
    setUploadExpectedName(undefined);
    setUploadOpen(true);
  };

  const handleOpenNewVersion = () => {
    if (!activeSkill) return;
    setUploadExpectedName(activeSkill.name);
    setUploadOpen(true);
  };

  if (skillsQuery.isLoading) {
    return (
      <div className="">
        <PageHeader title="Skills" description="Loading skills…" />
        <Skeleton className="mt-6 h-96 w-full" />
      </div>
    );
  }

  if (skillsQuery.isError) {
    const error = skillsQuery.error;
    return (
      <div className="">
        <ErrorState
          title="Could not load project skills"
          message={
            error instanceof Error ? error.message : 'An error occurred while loading skills.'
          }
          code={error instanceof ApiError ? error.code : undefined}
          requestId={error instanceof ApiError ? (error.requestId ?? undefined) : undefined}
          onRetry={() => void skillsQuery.refetch()}
        />
      </div>
    );
  }

  return (
    <div className="">
      <PageHeader
        title="Skills"
        description="Reusable agent capabilities, instructions, and tools configured for this project."
        actions={
          canUpload ? (
            <Button onClick={handleOpenNewSkill} className="gap-1.5">
              <Plus className="size-4" aria-hidden="true" />
              <span>New skill</span>
            </Button>
          ) : undefined
        }
      />

      {!canUpload ? (
        <div
          role="status"
          className="mt-4 rounded-md border border-border bg-surface-raised p-4 text-sm"
        >
          <h3 className="font-semibold text-fg">Sign in with email or Google to upload skills</h3>
          <p className="mt-1 text-xs text-fg-muted">
            You are currently signed in with a personal access token. Uploading skills requires
            signing in with email or Google.
          </p>
        </div>
      ) : null}

      {skills.length === 0 ? (
        <div className="mt-6">
          <SkillList skills={[]} />
          {canUpload ? (
            <div className="mt-4 flex justify-center">
              <Button onClick={handleOpenNewSkill} className="gap-1.5">
                <Plus className="size-4" aria-hidden="true" />
                <span>Upload your first skill</span>
              </Button>
            </div>
          ) : null}
        </div>
      ) : (
        <div className="mt-6">
          <SplitPane
            ariaLabel="Skills"
            fillViewport
            leftWidth="md:w-80 lg:w-96"
            leftClassName="p-3 sm:p-4"
            left={
              <section aria-labelledby="skills-list-heading">
                <ListPaneHeading id="skills-list-heading" title="Skills" count={skills.length} />
                <SkillList
                  skills={skills}
                  selectedName={activeSkill?.name}
                  onSelect={(skill) => setSelectedName(skill.name)}
                />
              </section>
            }
            right={
              detailQuery.isLoading && !detailQuery.data ? (
                <div className="space-y-4">
                  <Skeleton className="h-8 w-1/3" />
                  <Skeleton className="h-4 w-2/3" />
                  <Skeleton className="h-48 w-full" />
                </div>
              ) : activeSkill ? (
                <SkillDetail
                  skill={activeSkill}
                  skillMarkdown={skillMarkdown}
                  history={history}
                  describeUsage={describeUsage}
                  onNewVersion={canUpload ? handleOpenNewVersion : undefined}
                />
              ) : null
            }
          />
        </div>
      )}

      <UploadSkillDialog
        open={uploadOpen}
        onOpenChange={setUploadOpen}
        projectId={projectId}
        expectedName={uploadExpectedName}
        onSuccess={(uploaded) => {
          setSelectedName(uploaded.name);
        }}
      />
    </div>
  );
}
