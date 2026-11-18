import { useState } from 'react';
import { defineGallery } from '../../gallery/registry';
import { CommandPalette, type CommandPaletteItem } from './CommandPalette';
import { useCommandPaletteHotkey } from './useCommandPaletteHotkey';
import { Button } from '../primitives/button';
import { Play, RotateCcw, FileText, Search, Settings, HelpCircle } from 'lucide-react';

const sampleCommands: CommandPaletteItem[] = [
  {
    id: 'run-post-deploy',
    label: 'Run post-deploy validation',
    group: 'Recent',
    hint: '↵',
    icon: Play,
    onSelect: () => {},
  },
  {
    id: 'view-deploy-prod',
    label: 'View execution: Deploy API service v2.4.1',
    group: 'Recent',
    hint: 'exe_01JABC...',
    icon: Search,
    onSelect: () => {},
  },
  {
    id: 'action-rerun',
    label: 'Rerun last failed execution',
    group: 'Actions',
    hint: '⌘R',
    icon: RotateCcw,
    onSelect: () => {},
  },
  {
    id: 'action-new-runbook',
    label: 'Create new runbook',
    group: 'Actions',
    hint: '⌘N',
    icon: FileText,
    onSelect: () => {},
  },
  {
    id: 'nav-runbooks',
    label: 'Go to Runbooks',
    group: 'Navigation',
    hint: 'G R',
    onSelect: () => {},
  },
  {
    id: 'nav-executions',
    label: 'Go to Executions',
    group: 'Navigation',
    hint: 'G E',
    onSelect: () => {},
  },
  {
    id: 'nav-settings',
    label: 'Project Settings',
    group: 'Settings',
    hint: 'G S',
    icon: Settings,
    onSelect: () => {},
  },
  {
    id: 'help-docs',
    label: 'Documentation & Runbook spec',
    group: 'Help',
    icon: HelpCircle,
    onSelect: () => {},
  },
];

function CommandPaletteDemo() {
  const [open, setOpen] = useState(false);
  const [lastSelected, setLastSelected] = useState<string | null>(null);

  useCommandPaletteHotkey(open, setOpen);

  const items = sampleCommands.map((cmd) => ({
    ...cmd,
    onSelect: () => {
      setLastSelected(cmd.label);
    },
  }));

  return (
    <div className="flex flex-col gap-4 max-w-md">
      <div className="flex items-center gap-3">
        <Button
          variant="secondary"
          onClick={() => setOpen(true)}
          className="gap-2 text-xs"
        >
          <Search className="size-3.5 text-fg-muted" aria-hidden="true" />
          <span>Quick actions & search</span>
          <kbd className="ml-2 font-mono text-2xs bg-surface border border-border px-1.5 py-0.5 rounded text-fg-muted">
            ⌘K
          </kbd>
        </Button>
      </div>

      {lastSelected && (
        <div className="p-3 bg-surface border border-border rounded-lg text-xs font-mono text-fg">
          Selected: {lastSelected}
        </div>
      )}

      <CommandPalette
        open={open}
        onOpenChange={setOpen}
        items={items}
      />
    </div>
  );
}

export default [
  defineGallery({
    id: 'command-palette',
    group: 'Palette',
    title: 'CommandPalette',
    examples: [
      {
        name: 'Interactive Command Palette (Press ⌘K or click)',
        render: () => <CommandPaletteDemo />,
      },
    ],
  }),
];
