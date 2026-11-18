// UI-1: the first-runbook empty state. Two ways in, side by side: build it
// here (UI-4: blank or from a template, in the builder) and the exact words
// to give an AI agent that has The Runbook's skill
// (skills/therunbook/references/authoring.md).
import { Bot, Check, Copy, FilePlus2 } from 'lucide-react';
import { useState } from 'react';
import { toast } from 'sonner';
import { NewRunbookForm } from '../builder/NewRunbookForm';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../../ui/primitives/tabs';

/** The command and prompt an agent needs, for the CLI or MCP. */
export function agentInstructions(origin: string, project: string) {
  const prompt = [
    `Create a runbook in The Runbook project "${project}" for our post-deploy checks:`,
    'check the health endpoint, the error rate and p95 latency per region, then summarise.',
    'Follow the therunbook skill: write the YAML, validate it, save it as a draft',
    'and tell me the draft revision. Do not publish.',
  ].join('\n');
  return {
    cli: [
      '# Once: give your agent the skill and sign in',
      'runbook skill install --agent claude --therunbook',
      `runbook login ${origin}`,
      '',
      '# The agent validates and saves the draft with',
      'runbook spec validate post-deploy.yaml --json',
      `runbook push post-deploy.yaml --draft --project ${project}`,
    ].join('\n'),
    mcp: [
      `# Connect your agent's MCP client to`,
      `${origin}/mcp`,
      '',
      '# The agent validates and saves the draft with the tools',
      'validate_runbook',
      `save_runbook_draft  (project: ${project}, expected_revision: 0)`,
    ].join('\n'),
    prompt,
  };
}

function CopyBlock({ label, text }: { label: string; text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="relative">
      <pre
        aria-label={label}
        className="overflow-x-auto rounded-md border border-border bg-surface-raised p-3 pr-11 font-mono text-xs leading-relaxed whitespace-pre text-fg"
      >
        {text}
      </pre>
      <button
        type="button"
        aria-label={`Copy ${label.toLowerCase()}`}
        className="absolute top-2 right-2 inline-flex size-7 items-center justify-center rounded-md text-fg-muted hover:bg-hover hover:text-fg"
        onClick={() => {
          void navigator.clipboard?.writeText(text).then(
            () => {
              setCopied(true);
              toast.success(`${label} copied`);
              setTimeout(() => setCopied(false), 1500);
            },
            () => toast.error('Could not copy; select the text instead.'),
          );
        }}
      >
        {copied ? <Check className="size-3.5 text-mint-text" /> : <Copy className="size-3.5" />}
      </button>
    </div>
  );
}

export function AgentCreatePanel({ origin, project }: { origin: string; project: string }) {
  const { cli, mcp, prompt } = agentInstructions(origin, project);
  return (
    <div className="grid gap-3">
      <p className="text-sm text-fg-muted">
        Ask an agent that has The Runbook&apos;s skill. It writes the YAML, validates it and saves a
        draft for you to review and publish.
      </p>
      <CopyBlock label="Prompt" text={prompt} />
      <Tabs defaultValue="cli">
        <TabsList aria-label="How your agent connects">
          <TabsTrigger value="cli">CLI</TabsTrigger>
          <TabsTrigger value="mcp">MCP</TabsTrigger>
        </TabsList>
        <TabsContent value="cli" className="mt-2">
          <CopyBlock label="CLI commands" text={cli} />
        </TabsContent>
        <TabsContent value="mcp" className="mt-2">
          <CopyBlock label="MCP setup" text={mcp} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

export function RunbooksEmpty({
  projectId,
  projectRef,
  projectTitle,
  canWrite,
  origin = window.location.origin,
}: {
  projectId: string;
  projectRef: string;
  projectTitle: string;
  canWrite: boolean;
  origin?: string;
}) {
  return (
    <section
      aria-labelledby="runbooks-empty-title"
      className="rounded-lg border border-border bg-surface"
    >
      <div className="border-b border-border px-6 py-6">
        <p className="font-mono text-[10px] font-medium tracking-[0.12em] text-fg-muted uppercase">
          {projectTitle}
        </p>
        <h2 id="runbooks-empty-title" className="mt-1.5 text-md font-semibold text-fg">
          No runbooks yet
        </h2>
        <p className="mt-1 max-w-xl text-sm text-fg-muted">
          A runbook is a procedure your agents and people run the same way every time: a post-deploy
          check, an incident drill, a customer handover. Start one of two ways.
        </p>
      </div>
      <div className="grid md:grid-cols-2 md:divide-x md:divide-border">
        <div className="grid content-start gap-4 p-6">
          <h3 className="flex items-center gap-2 text-sm font-semibold text-fg">
            <FilePlus2 className="size-4 text-accent" aria-hidden="true" />
            Build it here
          </h3>
          {canWrite ? (
            <NewRunbookForm projectId={projectId} projectRef={projectRef} origin={origin} />
          ) : (
            <p className="text-sm text-fg-muted">
              Your token cannot create runbooks. Sign in with one that has runbooks:write, or ask a
              project editor.
            </p>
          )}
        </div>
        <div className="grid content-start gap-4 border-t border-border p-6 md:border-t-0">
          <h3 className="flex items-center gap-2 text-sm font-semibold text-fg">
            <Bot className="size-4 text-accent" aria-hidden="true" />
            Create with your AI agent
          </h3>
          <AgentCreatePanel origin={origin} project={projectRef} />
        </div>
      </div>
    </section>
  );
}
