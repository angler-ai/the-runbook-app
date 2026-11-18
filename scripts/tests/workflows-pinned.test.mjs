// Every third-party action in the repository's workflows is pinned to a
// full commit SHA, with the version it stands for as a comment (UPD1-20).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const dir = new URL("../../.github/workflows/", import.meta.url).pathname;

test("workflow actions are pinned to commit SHAs", () => {
  const files = readdirSync(dir).filter((f) => /\.ya?ml$/.test(f));
  assert.ok(files.length > 0);
  const unpinned = [];
  for (const file of files) {
    readFileSync(join(dir, file), "utf8")
      .split("\n")
      .forEach((line, i) => {
        const m = line.match(/^\s*-?\s*uses:\s*(\S+)(.*)$/);
        if (!m || m[1].startsWith("./")) return;
        if (!/@[0-9a-f]{40}$/.test(m[1]) || !/#\s*v\d+\.\d+\.\d+/.test(m[2])) {
          unpinned.push(`${file}:${i + 1}: ${line.trim()}`);
        }
      });
  }
  assert.deepEqual(unpinned, []);
});
