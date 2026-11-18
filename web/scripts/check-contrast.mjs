#!/usr/bin/env node

/**
 * Contrast checker for Runbook design tokens.
 * Evaluates WCAG 2.1 relative luminance and contrast ratios
 * for all text/background pairs in light and dark themes.
 *
 * Requirements:
 * - Body text: >= 4.5:1
 * - UI components/borders & large text: >= 3.0:1
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const TOKENS_PATH = path.resolve(__dirname, '../src/styles/tokens.css');

// Hard-coded list of critical text/background and UI pairs to verify
const CHECKS = [
  // Core text and accent
  { name: 'Primary text on background', fg: '--rb-color-text', bg: '--rb-color-bg', required: 4.5 },
  { name: 'Primary text on surface', fg: '--rb-color-text', bg: '--rb-color-surface', required: 4.5 },
  { name: 'Primary text on surface-raised', fg: '--rb-color-text', bg: '--rb-color-surface-raised', required: 4.5 },
  { name: 'Muted text on background', fg: '--rb-color-text-muted', bg: '--rb-color-bg', required: 4.5 },
  { name: 'Muted text on surface', fg: '--rb-color-text-muted', bg: '--rb-color-surface', required: 4.5 },
  { name: 'Muted text on surface-raised', fg: '--rb-color-text-muted', bg: '--rb-color-surface-raised', required: 4.5 },
  { name: 'Accent text on background', fg: '--rb-color-accent', bg: '--rb-color-bg', required: 4.5 },
  { name: 'Accent text on surface', fg: '--rb-color-accent', bg: '--rb-color-surface', required: 4.5 },
  { name: 'Accent button fg on accent', fg: '--rb-color-accent-fg', bg: '--rb-color-accent', required: 4.5 },

  // UI-1: Shelf surfaces with Angler colours
  { name: 'Primary text on sidebar', fg: '--rb-color-text', bg: '--rb-color-sidebar', required: 4.5 },
  { name: 'Muted text on sidebar', fg: '--rb-color-text-muted', bg: '--rb-color-sidebar', required: 4.5 },
  { name: 'Muted text on sidebar hover', fg: '--rb-color-text-muted', bg: '--rb-color-sidebar-hover', required: 4.5 },
  { name: 'Muted text on row hover', fg: '--rb-color-text-muted', bg: '--rb-color-hover', required: 4.5 },
  { name: 'Accent text on row hover', fg: '--rb-color-accent', bg: '--rb-color-hover', required: 4.5 },
  { name: 'Accent text on accent-subtle', fg: '--rb-color-accent', bg: '--rb-color-accent-subtle', required: 4.5 },
  { name: 'Primary text on accent-subtle', fg: '--rb-color-text', bg: '--rb-color-accent-subtle', required: 4.5 },
  { name: 'Accent hover fg on accent hover', fg: '--rb-color-accent-hover-fg', bg: '--rb-color-accent-hover', required: 4.5 },
  { name: 'Accent strong text on surface', fg: '--rb-color-accent-strong', bg: '--rb-color-surface', required: 4.5 },
  { name: 'Mint text on surface', fg: '--rb-color-accent-secondary-text', bg: '--rb-color-surface', required: 4.5 },
  { name: 'Mint text on background', fg: '--rb-color-accent-secondary-text', bg: '--rb-color-bg', required: 4.5 },
  { name: 'Danger button fg on danger', fg: '--rb-color-danger-fg', bg: '--rb-color-danger', required: 4.5 },
  { name: 'Danger text on surface', fg: '--rb-color-danger-text', bg: '--rb-color-surface', required: 4.5 },
  { name: 'Warning text on surface', fg: '--rb-color-warning-text', bg: '--rb-color-surface', required: 4.5 },
  { name: 'Focus ring on background', fg: '--rb-color-focus', bg: '--rb-color-bg', required: 3.0 },
  { name: 'Focus ring on sidebar', fg: '--rb-color-focus', bg: '--rb-color-sidebar', required: 3.0 },
  { name: 'Input border on sidebar', fg: '--rb-color-border-input', bg: '--rb-color-sidebar', required: 3.0 },

  // UI borders
  { name: 'Input border on surface', fg: '--rb-color-border-input', bg: '--rb-color-surface', required: 3.0 },
  { name: 'Input border on background', fg: '--rb-color-border-input', bg: '--rb-color-bg', required: 3.0 },

  // Status pills (foreground on pill background)
  { name: 'Status Queued', fg: '--rb-status-queued-fg', bg: '--rb-status-queued-bg', required: 4.5 },
  { name: 'Status Running', fg: '--rb-status-running-fg', bg: '--rb-status-running-bg', required: 4.5 },
  { name: 'Status Waiting', fg: '--rb-status-waiting-fg', bg: '--rb-status-waiting-bg', required: 4.5 },
  { name: 'Status Paused', fg: '--rb-status-paused-fg', bg: '--rb-status-paused-bg', required: 4.5 },
  { name: 'Status Succeeded', fg: '--rb-status-succeeded-fg', bg: '--rb-status-succeeded-bg', required: 4.5 },
  { name: 'Status Failed', fg: '--rb-status-failed-fg', bg: '--rb-status-failed-bg', required: 4.5 },
  { name: 'Status Cancelled', fg: '--rb-status-cancelled-fg', bg: '--rb-status-cancelled-bg', required: 4.5 },
  { name: 'Status Skipped', fg: '--rb-status-skipped-fg', bg: '--rb-status-skipped-bg', required: 4.5 },
  { name: 'Status Timed Out', fg: '--rb-status-timed-out-fg', bg: '--rb-status-timed-out-bg', required: 4.5 },
  { name: 'Status Expired', fg: '--rb-status-expired-fg', bg: '--rb-status-expired-bg', required: 4.5 },

  // Verdict tags
  { name: 'Verdict Pass (text)', fg: '--rb-verdict-pass-fg', bg: '--rb-verdict-pass-bg', required: 4.5 },
  { name: 'Verdict Pass (border on surface)', fg: '--rb-verdict-pass-border', bg: '--rb-color-surface', required: 3.0 },
  { name: 'Verdict Warn (text)', fg: '--rb-verdict-warn-fg', bg: '--rb-verdict-warn-bg', required: 4.5 },
  { name: 'Verdict Warn (border on surface)', fg: '--rb-verdict-warn-border', bg: '--rb-color-surface', required: 3.0 },
  { name: 'Verdict Fail (text)', fg: '--rb-verdict-fail-fg', bg: '--rb-verdict-fail-bg', required: 4.5 },
  { name: 'Verdict Fail (border on surface)', fg: '--rb-verdict-fail-border', bg: '--rb-color-surface', required: 3.0 },
  { name: 'Verdict None (text)', fg: '--rb-verdict-none-fg', bg: '--rb-verdict-none-bg', required: 4.5 },
  { name: 'Verdict None (border on surface)', fg: '--rb-verdict-none-border', bg: '--rb-color-surface', required: 3.0 },

  // Stale and amended tags
  { name: 'Stale (text)', fg: '--rb-stale-fg', bg: '--rb-stale-bg', required: 4.5 },
  { name: 'Stale (border on surface)', fg: '--rb-stale-border', bg: '--rb-color-surface', required: 3.0 },
  { name: 'Amended (text)', fg: '--rb-amended-fg', bg: '--rb-amended-bg', required: 4.5 },
  { name: 'Amended (border on surface)', fg: '--rb-amended-border', bg: '--rb-color-surface', required: 3.0 },
];

/**
 * Parse hex color (#rrggbb or #rgb) into { r, g, b } in 0..255.
 */
/** @param {string} hex */
function parseHex(hex) {
  let cleaned = hex.trim();
  if (!cleaned.startsWith('#')) {
    throw new Error(`Color must be hex starting with #: "${hex}"`);
  }
  cleaned = cleaned.slice(1);
  if (cleaned.length === 3) {
    cleaned = cleaned.split('').map((/** @type {string} */ c) => c + c).join('');
  }
  if (cleaned.length !== 6 || !/^[0-9a-fA-F]{6}$/.test(cleaned)) {
    throw new Error(`Invalid hex color: "${hex}"`);
  }
  const r = parseInt(cleaned.slice(0, 2), 16);
  const g = parseInt(cleaned.slice(2, 4), 16);
  const b = parseInt(cleaned.slice(4, 6), 16);
  return { r, g, b };
}

/**
 * WCAG 2.1 relative luminance for a single sRGB channel (0..255).
 */
/** @param {number} c */
function channelLuminance(c) {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

/**
 * Calculate WCAG 2.1 relative luminance:
 * L = 0.2126 * R + 0.7152 * G + 0.0722 * B
 */
/** @param {number} r @param {number} g @param {number} b */
function relativeLuminance(r, g, b) {
  return 0.2126 * channelLuminance(r) + 0.7152 * channelLuminance(g) + 0.0722 * channelLuminance(b);
}

/**
 * Calculate contrast ratio between two colors:
 * (L1 + 0.05) / (L2 + 0.05) where L1 is lighter.
 */
/** @param {string} hex1 @param {string} hex2 */
function contrastRatio(hex1, hex2) {
  const rgb1 = parseHex(hex1);
  const rgb2 = parseHex(hex2);
  const l1 = relativeLuminance(rgb1.r, rgb1.g, rgb1.b);
  const l2 = relativeLuminance(rgb2.r, rgb2.g, rgb2.b);
  const lighter = Math.max(l1, l2);
  const darker = Math.min(l1, l2);
  return (lighter + 0.05) / (darker + 0.05);
}

/**
 * Parse tokens.css into { light: Map, dark: Map }.
 */
/** @param {string} css */
function parseTokens(css) {
  const light = new Map();
  const dark = new Map();

  // Strip comments
  const cleanCss = css.replace(/\/\*[\s\S]*?\*\//g, '');

  // Extract root light tokens: everything in `:root { ... }` before dark theme blocks
  const rootMatch = cleanCss.match(/:root\s*\{([^}]+)\}/);
  if (rootMatch) {
    extractDeclarations(rootMatch[1] ?? '', light);
  }

  // Extract dark tokens from :root[data-theme="dark"] { ... }
  const darkMatch = cleanCss.match(/:root\[data-theme=["']dark["']\](?:\s*,\s*\[data-theme=["']dark["']\])?\s*\{([^}]+)\}/);
  if (darkMatch) {
    extractDeclarations(darkMatch[1] ?? '', dark);
  } else {
    // Or from @media (prefers-color-scheme: dark) { ... }
    const mediaMatch = cleanCss.match(/@media\s*\(\s*prefers-color-scheme:\s*dark\s*\)\s*\{[\s\S]*?:root[^{]*\{([^}]+)\}/);
    if (mediaMatch) {
      extractDeclarations(mediaMatch[1] ?? '', dark);
    }
  }

  return { light, dark };
}

/** @param {string} block @param {Map<string, string>} map */
function extractDeclarations(block, map) {
  const lines = block.split(';');
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const colonIdx = trimmed.indexOf(':');
    if (colonIdx === -1) continue;
    const prop = trimmed.slice(0, colonIdx).trim();
    const val = trimmed.slice(colonIdx + 1).trim();
    if (prop.startsWith('--rb-')) {
      map.set(prop, val);
    }
  }
}

function run() {
  if (!fs.existsSync(TOKENS_PATH)) {
    console.error(`Error: tokens file not found at ${TOKENS_PATH}`);
    process.exit(1);
  }

  const css = fs.readFileSync(TOKENS_PATH, 'utf-8');
  const { light, dark } = parseTokens(css);

  const results = [];
  let failures = 0;

  const themes = [
    { name: 'Light', map: light },
    { name: 'Dark', map: dark },
  ];

  for (const theme of themes) {
    for (const check of CHECKS) {
      const fgHex = theme.map.get(check.fg);
      const bgHex = theme.map.get(check.bg);

      if (!fgHex || !bgHex) {
        results.push({
          Theme: theme.name,
          Element: check.name,
          'FG Token': check.fg,
          'BG Token': check.bg,
          FG: fgHex || 'MISSING',
          BG: bgHex || 'MISSING',
          Ratio: 'N/A',
          Required: `${check.required}:1`,
          Status: 'FAIL (missing token)',
        });
        failures++;
        continue;
      }

      try {
        const ratio = contrastRatio(fgHex, bgHex);
        const pass = ratio >= check.required;
        if (!pass) failures++;

        results.push({
          Theme: theme.name,
          Element: check.name,
          'FG Token': check.fg,
          'BG Token': check.bg,
          FG: fgHex,
          BG: bgHex,
          Ratio: `${ratio.toFixed(2)}:1`,
          Required: `${check.required}:1`,
          Status: pass ? 'PASS' : 'FAIL',
        });
      } catch (err) {
        results.push({
          Theme: theme.name,
          Element: check.name,
          'FG Token': check.fg,
          'BG Token': check.bg,
          FG: fgHex,
          BG: bgHex,
          Ratio: 'ERR',
          Required: `${check.required}:1`,
          Status: `FAIL (${err instanceof Error ? err.message : String(err)})`,
        });
        failures++;
      }
    }
  }

  console.log('\n=== Runbook Design Token Contrast Check ===\n');
  console.table(results);

  console.log(`\nTotal checks: ${results.length} | Passed: ${results.length - failures} | Failed: ${failures}\n`);

  if (failures > 0) {
    console.error(`Contrast check failed with ${failures} failure(s).`);
    process.exit(1);
  } else {
    console.log('All contrast checks passed.');
    process.exit(0);
  }
}

run();
