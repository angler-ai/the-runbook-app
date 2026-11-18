import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import bootScript from '../../public/theme-boot.js?raw';
import {
  LEGACY_OVERRIDE_KEY,
  OVERRIDE_KEY,
  OVERRIDE_STYLE_ID,
  applyOverrideCss,
  applyPreference,
  contrast,
  deriveThemeVars,
  loadThemeOverride,
  overrideCss,
  readPreference,
  savePreference,
} from '.';

function metaResponse(body: unknown, status = 200) {
  return (async () =>
    new Response(JSON.stringify(body), {
      status,
      headers: { 'content-type': 'application/json' },
    })) as unknown as typeof fetch;
}

afterEach(() => {
  document.getElementById(OVERRIDE_STYLE_ID)?.remove();
  document.documentElement.removeAttribute('data-theme');
  localStorage.clear();
});

describe('contrast', () => {
  it('matches the WCAG figures the tokens rely on', () => {
    expect(contrast('#4338ca', '#ffffff')).toBeCloseTo(7.9, 1);
    expect(contrast('#0aa57b', '#ffffff')).toBeLessThan(4.5);
    expect(contrast('#087a5b', '#ffffff')).toBeGreaterThanOrEqual(4.5);
  });
});

describe('deriveThemeVars', () => {
  it('keeps an accent that already passes and derives its shades', () => {
    const vars = deriveThemeVars('light', { accent: '#0F766E' });
    expect(vars['--rb-color-accent']).toBe('#0f766e');
    expect(vars['--rb-color-accent-fg']).toBe('#ffffff');
    // Lighter hover with a foreground that still reads on it.
    const hover = vars['--rb-color-accent-hover']!;
    expect(contrast(vars['--rb-color-accent-hover-fg']!, hover)).toBeGreaterThanOrEqual(4.5);
    expect(
      contrast(vars['--rb-color-accent']!, vars['--rb-color-accent-subtle']!),
    ).toBeGreaterThanOrEqual(4.5);
  });

  it('darkens a light accent until it passes AA on light surfaces', () => {
    const vars = deriveThemeVars('light', { accent: '#ffd400' });
    const accent = vars['--rb-color-accent']!;
    expect(accent).not.toBe('#ffd400');
    for (const surface of ['#ffffff', '#f8fafc', '#f1f5f9']) {
      expect(contrast(accent, surface)).toBeGreaterThanOrEqual(4.5);
    }
  });

  it('lightens a dark accent until it passes AA on the dark ground', () => {
    const vars = deriveThemeVars('dark', { accent: '#1e3a8a' });
    expect(contrast(vars['--rb-color-accent']!, '#0e0f14')).toBeGreaterThanOrEqual(4.5);
    expect(
      contrast(vars['--rb-color-accent-fg']!, vars['--rb-color-accent']!),
    ).toBeGreaterThanOrEqual(4.5);
  });

  it('derives a readable text shade for the secondary accent', () => {
    const vars = deriveThemeVars('light', { accent_secondary: '#00cc99' });
    expect(vars['--rb-color-accent-secondary']).toBe('#00cc99');
    expect(contrast(vars['--rb-color-accent-secondary-text']!, '#ffffff')).toBeGreaterThanOrEqual(
      4.5,
    );
  });

  it('ignores anything that is not #rrggbb', () => {
    expect(deriveThemeVars('light', { accent: 'red', accent_secondary: '#abc' })).toEqual({});
  });

  // Every text/background pair the override emits reaches WCAG AA (4.5:1),
  // whatever accent an operator picks: every channel in steps of 0x11, plus
  // the edge cases that once fell short (#111100's hover read at 4.21:1).
  it('reaches 4.5:1 on every pair it emits, for any accent', () => {
    const SURFACES = {
      light: ['#ffffff', '#f8fafc', '#f1f5f9'],
      dark: ['#0e0f14', '#15161d', '#1a1c24', '#20222c'],
    } as const;
    const TEXT = { light: '#1a1a2e', dark: '#f1f5f9' } as const;
    const steps = Array.from({ length: 16 }, (_, i) => (i * 0x11).toString(16).padStart(2, '0'));
    const accents = ['#111100', '#000000', '#ffffff', '#808080', '#0f766e', '#ffd400'];
    for (const r of steps)
      for (const g of steps) for (const b of steps) accents.push(`#${r}${g}${b}`);
    const failures: string[] = [];
    for (const theme of ['light', 'dark'] as const) {
      for (const accent of accents) {
        const v = deriveThemeVars(theme, { accent, accent_secondary: accent });
        const pairs: [string, string, string][] = [
          ['accent-fg on accent', v['--rb-color-accent-fg']!, v['--rb-color-accent']!],
          ['hover-fg on hover', v['--rb-color-accent-hover-fg']!, v['--rb-color-accent-hover']!],
          ['accent on subtle', v['--rb-color-accent']!, v['--rb-color-accent-subtle']!],
          ['text on subtle', TEXT[theme], v['--rb-color-accent-subtle']!],
          ['running fg on bg', v['--rb-status-running-fg']!, v['--rb-status-running-bg']!],
        ];
        for (const surface of SURFACES[theme]) {
          pairs.push(
            [`accent on ${surface}`, v['--rb-color-accent']!, surface],
            [`strong on ${surface}`, v['--rb-color-accent-strong']!, surface],
            [`secondary text on ${surface}`, v['--rb-color-accent-secondary-text']!, surface],
          );
        }
        for (const [name, fg, bg] of pairs) {
          const ratio = contrast(fg, bg);
          if (!(ratio >= 4.5)) failures.push(`${theme} ${accent} ${name}: ${ratio.toFixed(2)}`);
        }
      }
    }
    expect(failures.slice(0, 10)).toEqual([]);
    expect(failures).toHaveLength(0);
  });
});

describe('overrideCss', () => {
  it('is empty without an override', () => {
    expect(overrideCss(null)).toBe('');
    expect(overrideCss({ light: {}, dark: {} })).toBe('');
  });

  it('scopes dark values to the dark theme selectors', () => {
    const css = overrideCss({ light: { accent: '#0f766e' }, dark: { accent: '#5eead4' } });
    expect(css).toContain(':root{--rb-color-accent:#0f766e;');
    expect(css).toContain(
      '@media screen and (prefers-color-scheme: dark){:root:not([data-theme="light"]){',
    );
    expect(css).toContain('@media screen{:root[data-theme="dark"]{--rb-color-accent:#5eead4;');
  });
});

describe('theme preference', () => {
  it('defaults to light and remembers a choice', () => {
    expect(readPreference()).toBe('light');
    savePreference('dark');
    expect(readPreference()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('follows the system by leaving data-theme unset', () => {
    applyPreference('dark');
    applyPreference('system');
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });
});

describe('loadThemeOverride', () => {
  beforeEach(() => localStorage.clear());

  it('applies the override from /v1/meta and caches its colours, not CSS', async () => {
    const theme = { light: { accent: '#0f766e' }, dark: { accent: '#5eead4' } };
    localStorage.setItem(LEGACY_OVERRIDE_KEY, ':root{--rb-color-accent:#0f766e;}');
    await loadThemeOverride({ fetchImpl: metaResponse({ edition: 'self_hosted', theme }) });
    const style = document.getElementById(OVERRIDE_STYLE_ID);
    expect(style?.textContent).toBe(overrideCss(theme));
    const cached = JSON.parse(localStorage.getItem(OVERRIDE_KEY)!) as Record<
      string,
      Record<string, string>
    >;
    expect(cached).toEqual({
      light: deriveThemeVars('light', theme.light),
      dark: deriveThemeVars('dark', theme.dark),
    });
    for (const vars of Object.values(cached))
      for (const [name, value] of Object.entries(vars)) {
        expect(name).toMatch(/^--rb-[a-z0-9-]+$/);
        expect(value).toMatch(/^#[0-9a-f]{6}$/);
      }
    // The old CSS cache is gone.
    expect(localStorage.getItem(LEGACY_OVERRIDE_KEY)).toBeNull();
  });

  it('removes a cached override the server no longer sets', async () => {
    applyOverrideCss(':root{--rb-color-accent:#0f766e;}');
    localStorage.setItem(OVERRIDE_KEY, '{"light":{"--rb-color-accent":"#0f766e"},"dark":{}}');
    await loadThemeOverride({ fetchImpl: metaResponse({ edition: 'self_hosted' }) });
    expect(document.getElementById(OVERRIDE_STYLE_ID)).toBeNull();
    expect(localStorage.getItem(OVERRIDE_KEY)).toBeNull();
  });

  it('keeps the cached override when the server cannot be reached', async () => {
    applyOverrideCss(':root{--rb-color-accent:#0f766e;}');
    await loadThemeOverride({
      fetchImpl: (async () => {
        throw new TypeError('offline');
      }) as unknown as typeof fetch,
    });
    expect(document.getElementById(OVERRIDE_STYLE_ID)?.textContent).toContain('#0f766e');
  });
});

// public/theme-boot.js runs before the app (a classic script in <head>). It
// rebuilds the override from the cached colours, validating each one again,
// so whatever else sits in localStorage never reaches a <style> element.
describe('theme-boot.js', () => {
  function boot() {
    new Function(bootScript)();
    return document.getElementById(OVERRIDE_STYLE_ID)?.textContent ?? null;
  }

  it('applies the preference and builds exactly the CSS overrideCss does', () => {
    const theme = {
      light: { accent: '#0f766e', accent_secondary: '#00cc99' },
      dark: { accent: '#5eead4' },
    };
    localStorage.setItem('rb.theme', 'dark');
    localStorage.setItem(
      OVERRIDE_KEY,
      JSON.stringify({
        light: deriveThemeVars('light', theme.light),
        dark: deriveThemeVars('dark', theme.dark),
      }),
    );
    expect(boot()).toBe(overrideCss(theme));
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('matches overrideCss for a light-only and a dark-only override', () => {
    for (const theme of [
      { light: { accent: '#111100' } },
      { dark: { accent_secondary: '#808080' } },
    ]) {
      document.getElementById(OVERRIDE_STYLE_ID)?.remove();
      localStorage.setItem(
        OVERRIDE_KEY,
        JSON.stringify({
          light: deriveThemeVars('light', theme.light ?? {}),
          dark: deriveThemeVars('dark', theme.dark ?? {}),
        }),
      );
      expect(boot()).toBe(overrideCss(theme));
    }
  });

  it('drops every name and value that is not a colour token', () => {
    localStorage.setItem(
      OVERRIDE_KEY,
      JSON.stringify({
        light: {
          '--rb-color-accent': '#0f766e',
          '--rb-color-focus': '#000000}body{background:url(https://evil.example/x)}',
          '--rb-x}</style><script>alert(1)</script>': '#aabbcc',
          '--rb-color-accent-fg': 'url(javascript:alert(1))',
          '--RB-UPPER': '#aabbcc',
          color: '#aabbcc',
          '--rb-color-text': '#AABBCC',
          '--rb-color-hover': 123,
        },
        dark: '</style><script>alert(1)</script>',
      }),
    );
    const css = boot();
    expect(css).toBe(':root{--rb-color-accent:#0f766e;}');
  });

  it('applies nothing from a cache that is not JSON, nor from the old CSS key', () => {
    localStorage.setItem(OVERRIDE_KEY, '}</style><script>alert(1)</script>');
    localStorage.setItem(
      LEGACY_OVERRIDE_KEY,
      'body{background:url(https://evil.example/x)}</style>',
    );
    expect(boot()).toBeNull();
    expect(localStorage.getItem(LEGACY_OVERRIDE_KEY)).toBeNull();
  });
});
