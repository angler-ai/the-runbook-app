import assert from 'node:assert/strict';
import { test } from 'node:test';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

test('manifest.webmanifest parses and has required fields', () => {
  const manifestPath = path.resolve(__dirname, '../../../public/manifest.webmanifest');
  assert.equal(fs.existsSync(manifestPath), true, 'manifest.webmanifest must exist');

  const raw = fs.readFileSync(manifestPath, 'utf8');
  const manifest = JSON.parse(raw);

  assert.equal(manifest.name, 'The Runbook');
  assert.equal(manifest.short_name, 'Runbook');
  assert.equal(manifest.start_url, '/');
  assert.equal(manifest.display, 'standalone');
  assert.equal(manifest.theme_color, '#FFFFFF');
  assert.equal(manifest.background_color, '#F8FAFC');

  assert.equal(Array.isArray(manifest.icons), true);
  assert.ok(manifest.icons.length >= 1);

  for (const icon of manifest.icons) {
    assert.ok(icon.src, 'icon src required');
    assert.ok(icon.type, 'icon type required');
    assert.ok(icon.sizes, 'icon sizes required');
    assert.equal(icon.purpose, 'any');

    const iconPath = path.resolve(__dirname, '../../../public', icon.src.replace(/^\//, ''));
    assert.equal(fs.existsSync(iconPath), true, `Icon file does not exist at ${iconPath}`);
  }
});
