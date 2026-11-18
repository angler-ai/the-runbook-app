import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, renderHook } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expectNoAxeViolations } from '../../test/axe';
import { CommandPalette, type CommandPaletteItem } from './CommandPalette';
import { useCommandPaletteHotkey, isInputElement } from './useCommandPaletteHotkey';

describe('useCommandPaletteHotkey', () => {
  it('identifies input and textarea elements correctly', () => {
    const input = document.createElement('input');
    const textarea = document.createElement('textarea');
    const div = document.createElement('div');
    const contentEditable = document.createElement('div');
    contentEditable.contentEditable = 'true';

    expect(isInputElement(input)).toBe(true);
    expect(isInputElement(textarea)).toBe(true);
    expect(isInputElement(contentEditable)).toBe(true);
    expect(isInputElement(div)).toBe(false);
    expect(isInputElement(null)).toBe(false);
  });

  it('triggers on ⌘K and Ctrl+K when not typing in input', () => {
    let open = false;
    const setOpen = vi.fn((val) => {
      open = typeof val === 'function' ? val(open) : val;
    });

    renderHook(() => useCommandPaletteHotkey(open, setOpen));

    // ⌘K on body
    const metaKEvent = new KeyboardEvent('keydown', {
      key: 'k',
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    window.dispatchEvent(metaKEvent);
    expect(setOpen).toHaveBeenCalled();

    // Ctrl+K on body
    const ctrlKEvent = new KeyboardEvent('keydown', {
      key: 'K',
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    window.dispatchEvent(ctrlKEvent);
    expect(setOpen).toHaveBeenCalledTimes(2);
  });

  it('does NOT trigger while typing in an input when palette is closed', () => {
    const setOpen = vi.fn();
    renderHook(() => useCommandPaletteHotkey(false, setOpen));

    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();

    const event = new KeyboardEvent('keydown', {
      key: 'k',
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    input.dispatchEvent(event);

    expect(setOpen).not.toHaveBeenCalled();
    document.body.removeChild(input);
  });

  it('DOES trigger when palette is already open, even if focused in an input', () => {
    const setOpen = vi.fn();
    renderHook(() => useCommandPaletteHotkey(true, setOpen));

    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();

    const event = new KeyboardEvent('keydown', {
      key: 'k',
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    input.dispatchEvent(event);

    expect(setOpen).toHaveBeenCalled();
    document.body.removeChild(input);
  });
});

describe('CommandPalette', () => {
  const sampleItems: CommandPaletteItem[] = [
    {
      id: 'cmd-1',
      label: 'Run post-deploy validation',
      group: 'Recent',
      hint: '↵',
      onSelect: vi.fn(),
    },
    {
      id: 'cmd-2',
      label: 'Create new runbook',
      group: 'Actions',
      hint: '⌘N',
      onSelect: vi.fn(),
    },
    {
      id: 'cmd-3',
      label: 'Go to Runbooks',
      group: 'Navigation',
      hint: 'G R',
      onSelect: vi.fn(),
    },
  ];

  it('renders open dialog with groups, items, footer, and is axe-clean', async () => {
    const onOpenChange = vi.fn();

    const { container } = render(
      <CommandPalette
        open={true}
        onOpenChange={onOpenChange}
        items={sampleItems}
      />,
    );

    // Dialog accessibility
    const dialog = screen.getByRole('dialog', { name: 'Command Palette' });
    expect(dialog).toBeInTheDocument();

    // Groups & items
    expect(screen.getByText('Recent')).toBeInTheDocument();
    expect(screen.getByText('Run post-deploy validation')).toBeInTheDocument();

    expect(screen.getByText('Actions')).toBeInTheDocument();
    expect(screen.getByText('Create new runbook')).toBeInTheDocument();
    expect(screen.getByText('⌘N')).toBeInTheDocument();

    expect(screen.getByText('Navigation')).toBeInTheDocument();
    expect(screen.getByText('Go to Runbooks')).toBeInTheDocument();

    // Keyboard hints footer
    expect(screen.getByText('to select')).toBeInTheDocument();
    expect(screen.getByText('to navigate')).toBeInTheDocument();
    expect(screen.getByText('to close')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('calls onSelect and closes when item is clicked', async () => {
    const user = userEvent.setup();
    const onSelectAction = vi.fn();
    const onOpenChange = vi.fn();

    const items: CommandPaletteItem[] = [
      {
        id: 'cmd-test',
        label: 'Trigger Incident',
        group: 'Actions',
        onSelect: onSelectAction,
      },
    ];

    render(
      <CommandPalette
        open={true}
        onOpenChange={onOpenChange}
        items={items}
      />,
    );

    const item = screen.getByText('Trigger Incident');
    await user.click(item);

    expect(onSelectAction).toHaveBeenCalledTimes(1);
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it('filters items when search input is typed', async () => {
    const user = userEvent.setup();

    render(
      <CommandPalette
        open={true}
        onOpenChange={vi.fn()}
        items={sampleItems}
      />,
    );

    const searchInput = screen.getByPlaceholderText('Type a command or search...');
    await user.type(searchInput, 'validation');

    expect(screen.getByText('Run post-deploy validation')).toBeInTheDocument();
    expect(screen.queryByText('Create new runbook')).toBeNull();
  });

  it('closes when Escape key is pressed', () => {
    const onOpenChange = vi.fn();

    render(
      <CommandPalette
        open={true}
        onOpenChange={onOpenChange}
        items={sampleItems}
      />,
    );

    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape', code: 'Escape' });
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  // UI-1: the selected row sits on the accent's subtle tint (Shelf), where
  // muted text still reads (5.2:1 light, checked by scripts/check-contrast.mjs).
  it('keeps the hint in muted text on the selected tint', () => {
    render(
      <CommandPalette
        open={true}
        onOpenChange={vi.fn()}
        items={sampleItems}
      />,
    );

    const hints = document.querySelectorAll('[data-slot="command-palette-item-hint"]');
    expect(hints.length).toBeGreaterThan(0);
    hints.forEach((hint) => {
      expect(hint).toHaveClass('text-fg-muted');
      expect(hint.closest('[cmdk-item]')).toHaveClass('data-[selected=true]:bg-accent-subtle');
    });
  });
});
