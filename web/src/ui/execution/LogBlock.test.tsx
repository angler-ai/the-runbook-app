import { act, render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { LOG_ROW_HEIGHT, LogBlock } from './LogBlock';

describe('LogBlock', () => {
  it('stays collapsed until opened, and asks for the log once', async () => {
    const onOpen = vi.fn();
    const { container } = render(<LogBlock lines={['one', 'two']} onOpen={onOpen} />);
    expect(screen.queryByRole('region', { name: 'Log lines' })).not.toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: /show log/i }));
    expect(screen.getByRole('region', { name: 'Log lines' })).toHaveTextContent('one');
    await user.click(screen.getByRole('button', { name: /hide log/i }));
    await user.click(screen.getByRole('button', { name: /show log/i }));
    expect(onOpen).toHaveBeenCalledTimes(1);
    await expectNoAxeViolations(container);
  });

  it('renders only the rows in view for a long log', () => {
    const lines = Array.from({ length: 5000 }, (_, i) => `line ${i + 1}`);
    render(<LogBlock lines={lines} defaultOpen />);
    const region = screen.getByRole('region', { name: 'Log lines' });
    expect(region).toHaveAttribute('data-virtual', 'true');
    expect(region.querySelectorAll('[data-slot="log-line"]').length).toBeLessThan(100);
    expect(region).toHaveTextContent('line 1');
    // Scrolling far down brings those rows in and drops the first ones.
    Object.defineProperty(region, 'scrollTop', {
      value: 4000 * LOG_ROW_HEIGHT,
      configurable: true,
    });
    fireEvent.scroll(region);
    expect(region).toHaveTextContent('line 4001');
    expect(region).not.toHaveTextContent(/^line 1line/);
    expect(region.querySelectorAll('[data-slot="log-line"]').length).toBeLessThan(100);
  });

  it('copies the whole log', async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    render(<LogBlock lines={['a', 'b']} defaultOpen />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('a\nb'));
    expect(await screen.findByRole('button', { name: 'Copied' })).toBeInTheDocument();
  });

  it('copies every line, starting the write inside the click while it loads the rest (UI-3 re-check)', async () => {
    // The write must start before any await (Safari refuses a clipboard
    // write outside the click), with the text a pending Blob.
    let written: Promise<string> | undefined;
    const write = vi.fn((items: { 'text/plain': Promise<Blob> }[]) => {
      written = items[0]!['text/plain'].then((blob) => blob.text());
      return written.then(() => undefined);
    });
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { write, writeText }, configurable: true });
    // The fake write reads the Blob promise straight off the item.
    function FakeClipboardItem(items: Record<string, Promise<Blob>>) {
      return items;
    }
    vi.stubGlobal('ClipboardItem', FakeClipboardItem);
    const onCopyAll = vi.fn(() => Promise.resolve({ lines: ['a', 'b', 'c'], complete: true }));
    render(<LogBlock lines={['a', 'b']} hasMore onCopyAll={onCopyAll} defaultOpen />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    expect(write).toHaveBeenCalledTimes(1);
    expect(await written).toBe('a\nb\nc');
    expect(await screen.findByRole('button', { name: 'Copied' })).toBeInTheDocument();
    expect(writeText).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it('copies the loaded lines and says so where the clipboard cannot wait for the rest', async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const onCopyAll = vi.fn(() => Promise.resolve({ lines: ['a', 'b', 'c'], complete: true }));
    render(<LogBlock lines={['a', 'b']} hasMore onCopyAll={onCopyAll} defaultOpen />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('a\nb'));
    expect(
      await screen.findByRole('button', { name: 'Copied the 2 loaded lines' }),
    ).toBeInTheDocument();
  });

  it('says when the copy failed', async () => {
    const writeText = vi.fn(() => Promise.reject(new Error('denied')));
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    render(<LogBlock lines={['a']} defaultOpen />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    expect(await screen.findByRole('button', { name: 'Copy failed' })).toBeInTheDocument();
  });

  it('says what it copied when only part of the log is loaded', async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    render(<LogBlock lines={['a', 'b']} hasMore defaultOpen />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('a\nb'));
    expect(
      await screen.findByRole('button', { name: 'Copied the 2 loaded lines' }),
    ).toBeInTheDocument();
  });

  it('prints every line of an open log, and says where a closed one is', () => {
    const lines = Array.from({ length: 5000 }, (_, i) => `line ${i + 1}`);
    const { unmount } = render(<LogBlock lines={lines} defaultOpen />);
    const region = screen.getByRole('region', { name: 'Log lines' });
    expect(region.querySelectorAll('[data-slot="log-line"]').length).toBeLessThan(100);
    // No act(): the lines must be in the DOM when `beforeprint` returns,
    // as the browser snapshots the page right after it (UI-3 re-check).
    window.dispatchEvent(new Event('beforeprint'));
    expect(region.querySelectorAll('[data-slot="log-line"]').length).toBe(5000);
    act(() => {
      window.dispatchEvent(new Event('afterprint'));
    });
    expect(region.querySelectorAll('[data-slot="log-line"]').length).toBeLessThan(100);
    unmount();
    const { container } = render(<LogBlock lines={[]} printNote="Log on the run page" />);
    const note = container.querySelector('[data-slot="log-print-note"]');
    expect(note).toHaveTextContent('Log on the run page');
    expect(note).toHaveClass('hidden', 'print:block');
  });
});
