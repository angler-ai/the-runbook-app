// UI-3 review: an operator's dark accent applies on screen only, like the
// dark tokens (print.node.test.ts checks the stylesheets themselves).
import { describe, expect, it } from 'vitest';
import bootScript from '../../public/theme-boot.js?raw';
import { overrideCss } from '../theme/colors';

describe('print uses the light theme', () => {
  it('scopes an operator’s dark accent to the screen too', () => {
    const css = overrideCss({ light: { accent: '#0f766e' }, dark: { accent: '#5eead4' } });
    expect(css).toContain('@media screen and (prefers-color-scheme: dark){');
    expect(css).toContain('@media screen{:root[data-theme="dark"]{--rb-color-accent:#5eead4;');
    expect(bootScript).toContain("'@media screen and (prefers-color-scheme: dark){");
    expect(bootScript).toContain('\'@media screen{:root[data-theme="dark"]{\'');
  });
});
