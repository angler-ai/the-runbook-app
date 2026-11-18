import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from './alert-dialog';
import { Button } from './button';
import { Checkbox } from './checkbox';
import { cn } from './cn';
import { Dialog, DialogContent, DialogDescription, DialogTitle, DialogTrigger } from './dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from './dropdown-menu';
import { Label } from './label';
import { Tabs, TabsContent, TabsList, TabsTrigger } from './tabs';

describe('cn', () => {
  it('lets later utilities win and keeps token colors and sizes apart', () => {
    expect(cn('h-9 px-4', 'px-2')).toBe('h-9 px-2');
    expect(cn('text-fg text-base', 'text-xs')).toBe('text-fg text-xs');
  });
});

describe('Button', () => {
  it('defaults to type=button and is axe clean', async () => {
    const { container } = render(
      <>
        <Button>Start run</Button>
        <Button variant="destructive">Roll back</Button>
      </>,
    );
    expect(screen.getByRole('button', { name: 'Start run' })).toHaveAttribute('type', 'button');
    await expectNoAxeViolations(container);
  });

  it('renders its child with asChild', () => {
    render(
      <Button asChild>
        <a href="https://example.test/report">Report</a>
      </Button>,
    );
    expect(screen.getByRole('link', { name: 'Report' })).toHaveAttribute('data-slot', 'button');
  });
});

describe('Dialog', () => {
  it('opens a labelled dialog and closes on Escape', async () => {
    const user = userEvent.setup();
    render(
      <Dialog>
        <DialogTrigger asChild>
          <Button>Start run</Button>
        </DialogTrigger>
        <DialogContent>
          <DialogTitle>Start post-deploy</DialogTitle>
          <DialogDescription>Fill in the inputs.</DialogDescription>
        </DialogContent>
      </Dialog>,
    );
    await user.click(screen.getByRole('button', { name: 'Start run' }));
    expect(screen.getByRole('dialog', { name: 'Start post-deploy' })).toBeInTheDocument();
    await expectNoAxeViolations(document.body);
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

describe('AlertDialog', () => {
  it('requires an explicit choice for destructive actions', async () => {
    const user = userEvent.setup();
    let confirmed = false;
    render(
      <AlertDialog>
        <AlertDialogTrigger asChild>
          <Button variant="destructive">Roll back</Button>
        </AlertDialogTrigger>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Roll back the deployment?</AlertDialogTitle>
            <AlertDialogDescription>This cannot be undone.</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep deployment</AlertDialogCancel>
            <AlertDialogAction destructive onClick={() => (confirmed = true)}>
              Roll back
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>,
    );
    await user.click(screen.getByRole('button', { name: 'Roll back' }));
    const dialog = screen.getByRole('alertdialog', { name: 'Roll back the deployment?' });
    await expectNoAxeViolations(document.body);
    await user.click(screen.getAllByRole('button', { name: 'Roll back' }).at(-1)!);
    expect(confirmed).toBe(true);
    expect(dialog).not.toBeInTheDocument();
  });
});

describe('Checkbox', () => {
  it('toggles through its label', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <div>
        <Checkbox id="smoke" />
        <Label htmlFor="smoke">Smoke test passed</Label>
      </div>,
    );
    const box = screen.getByRole('checkbox', { name: 'Smoke test passed' });
    expect(box).not.toBeChecked();
    await user.click(screen.getByText('Smoke test passed'));
    expect(box).toBeChecked();
    await expectNoAxeViolations(container);
  });
});

describe('Tabs', () => {
  it('moves between tabs with the arrow keys', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <Tabs defaultValue="yaml">
        <TabsList aria-label="Editor views">
          <TabsTrigger value="yaml">YAML</TabsTrigger>
          <TabsTrigger value="graph">Graph</TabsTrigger>
        </TabsList>
        <TabsContent value="yaml">YAML view</TabsContent>
        <TabsContent value="graph">Graph view</TabsContent>
      </Tabs>,
    );
    await user.click(screen.getByRole('tab', { name: 'YAML' }));
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Graph' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tabpanel')).toHaveTextContent('Graph view');
    await expectNoAxeViolations(container);
  });
});

describe('DropdownMenu', () => {
  it('opens a menu from the keyboard', async () => {
    const user = userEvent.setup();
    render(
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="secondary">Actions</Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem>Pause</DropdownMenuItem>
          <DropdownMenuItem destructive>Cancel run</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>,
    );
    screen.getByRole('button', { name: 'Actions' }).focus();
    await user.keyboard('{Enter}');
    expect(screen.getByRole('menuitem', { name: 'Cancel run' })).toBeInTheDocument();
    await expectNoAxeViolations(document.body);
  });
});
