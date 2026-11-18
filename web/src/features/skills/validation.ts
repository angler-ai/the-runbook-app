import YAML from 'yaml';

export const MAX_PATH_BYTES = 255;
export const DEFAULT_MAX_BUNDLE_BYTES = 10 * 1024 * 1024; // 10 MB

export interface ValidatedFile {
  path: string;
  size: number;
  content: string; // base64 string
}

export interface SkillValidationResult {
  name: string;
  description: string;
  files: ValidatedFile[];
  totalBytes: number;
}

export function validateSkillPath(path: string): string | null {
  if (!path) return 'empty';
  const encoder = new TextEncoder();
  if (encoder.encode(path).length > MAX_PATH_BYTES) return 'too_long';
  if (path.startsWith('/')) return 'absolute';
  if (path.includes('\\')) return 'backslash';
  for (let i = 0; i < path.length; i++) {
    const code = path.charCodeAt(i);
    if (code <= 0x1f || (code >= 0x7f && code <= 0x9f)) {
      return 'control_character';
    }
  }
  const parts = path.split('/');
  for (const part of parts) {
    if (part === '') return 'empty_component';
    if (part === '.' || part === '..') return 'dot_component';
  }
  return null;
}

export function parseSkillFrontmatter(content: string): { name: string; description: string } {
  const text = content.replace(/^\uFEFF/, '');
  if (!text.startsWith('---\n') && !text.startsWith('---\r\n')) {
    throw new Error("SKILL.md is missing opening frontmatter delimiter '---'");
  }
  const rest = text.startsWith('---\r\n') ? text.slice(5) : text.slice(4);
  const lines = rest.split('\n');
  let endOffset = -1;
  let currentOffset = 0;
  for (const line of lines) {
    const trimmed = line.replace(/\r$/, '');
    if (trimmed === '---') {
      endOffset = currentOffset;
      break;
    }
    currentOffset += line.length + 1;
  }
  if (endOffset === -1) {
    throw new Error("SKILL.md is missing closing frontmatter delimiter '---'");
  }
  const frontmatterStr = rest.slice(0, endOffset);
  let docs: unknown;
  try {
    docs = YAML.parse(frontmatterStr);
  } catch (e) {
    throw new Error(
      `SKILL.md frontmatter is invalid YAML: ${e instanceof Error ? e.message : String(e)}`,
    );
  }
  if (!docs || typeof docs !== 'object' || Array.isArray(docs)) {
    throw new Error('SKILL.md frontmatter must be a YAML mapping');
  }
  const map = docs as Record<string, unknown>;
  const name = typeof map.name === 'string' ? map.name : undefined;
  if (!name) {
    throw new Error("SKILL.md frontmatter lacks 'name'");
  }
  const validName =
    name.length > 0 &&
    name.length <= 64 &&
    /^[a-z0-9-]+$/.test(name) &&
    !name.startsWith('-') &&
    !name.endsWith('-');
  if (!validName) {
    throw new Error(
      `skill name '${name}' is invalid: must be lowercase letters, digits, and hyphens`,
    );
  }
  const description = typeof map.description === 'string' ? map.description : undefined;
  if (!description) {
    throw new Error("SKILL.md frontmatter lacks 'description'");
  }
  if (description.trim().length === 0 || description.length > 1024) {
    throw new Error('SKILL.md description must be non-empty and at most 1024 characters');
  }
  return { name, description };
}

export function uint8ArrayToBase64(bytes: Uint8Array): string {
  const chunkSize = 0x8000;
  let binary = '';
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode.apply(null, chunk as unknown as number[]);
  }
  return btoa(binary);
}

export interface InputSkillFile {
  file: File;
  relativePath?: string;
}

export async function validateSkillFiles(
  inputFiles: InputSkillFile[],
  options: {
    maxBytes?: number;
    expectedName?: string;
  } = {},
): Promise<SkillValidationResult> {
  const maxBytes = options.maxBytes ?? DEFAULT_MAX_BUNDLE_BYTES;
  if (!inputFiles || inputFiles.length === 0) {
    throw new Error('No files provided');
  }

  const rawEntries = inputFiles.map((item) => {
    const rawPath = item.relativePath || item.file.webkitRelativePath || item.file.name;
    const normalized = rawPath.replace(/\\/g, '/').replace(/^\/+/, '');
    return {
      file: item.file,
      rawPath: normalized,
    };
  });

  const pathParts = rawEntries.map((e) => e.rawPath.split('/'));
  let folderName: string | undefined;
  const firstParts = pathParts[0];
  if (firstParts && firstParts.length > 1) {
    const candidate = firstParts[0];
    if (candidate && pathParts.every((parts) => parts.length > 1 && parts[0] === candidate)) {
      folderName = candidate;
    }
  }

  const entries = rawEntries.map((e) => {
    let relPath = e.rawPath;
    if (folderName && relPath.startsWith(`${folderName}/`)) {
      relPath = relPath.slice(folderName.length + 1);
    }
    return {
      file: e.file,
      relPath,
    };
  });

  const skillMdEntry = entries.find((e) => e.relPath.toLowerCase() === 'skill.md');
  if (!skillMdEntry) {
    const folderDisplay = folderName ? `'${folderName}'` : 'skill folder';
    throw new Error(`SKILL.md is missing in ${folderDisplay}`);
  }

  if (skillMdEntry.relPath !== 'SKILL.md') {
    throw new Error("SKILL.md must be uppercase 'SKILL.md'");
  }

  const skillMdText = await skillMdEntry.file.text();
  const { name, description } = parseSkillFrontmatter(skillMdText);

  if (folderName && name !== folderName) {
    throw new Error(`skill name '${name}' does not match folder name '${folderName}'`);
  }

  if (options.expectedName && name !== options.expectedName) {
    throw new Error(`skill name '${name}' does not match expected skill '${options.expectedName}'`);
  }

  const seenPaths = new Set<string>();
  let totalBytes = 0;
  const validatedFiles: ValidatedFile[] = [];

  for (const entry of entries) {
    const pathErr = validateSkillPath(entry.relPath);
    if (pathErr) {
      throw new Error(`invalid file path '${entry.relPath}': ${pathErr}`);
    }
    if (seenPaths.has(entry.relPath)) {
      throw new Error(`invalid skill bundle: duplicate path '${entry.relPath}'`);
    }
    seenPaths.add(entry.relPath);

    totalBytes += entry.file.size;
    const buffer = await entry.file.arrayBuffer();
    const bytes = new Uint8Array(buffer);
    const base64Content = uint8ArrayToBase64(bytes);

    validatedFiles.push({
      path: entry.relPath,
      size: entry.file.size,
      content: base64Content,
    });
  }

  if (totalBytes > maxBytes) {
    throw new Error(
      `skill bundle size (${totalBytes} bytes) exceeds server limit (${maxBytes} bytes)`,
    );
  }

  return {
    name,
    description,
    files: validatedFiles,
    totalBytes,
  };
}
