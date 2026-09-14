import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, extname, dirname, resolve, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

// An allowlist of roots rather than the package root with a denylist. A
// denylist would have to name node_modules, dist, release, resources,
// src-tauri, target, tests and playwright-report, and it would rot; this way
// adding a third root is a one-line, obvious edit, and the scan still cannot
// wander into node_modules by accident.
const SCAN_ROOTS = [HERE, resolve(HERE, "../electron")];

function collectSourceFiles(dir: string): string[] {
  const result: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      result.push(...collectSourceFiles(full));
    } else if ([".ts", ".tsx"].includes(extname(entry))) {
      result.push(full);
    }
  }
  return result;
}

// Matches any Unicode extended pictographic (covers all emoji and pictographic symbols)
const EMOJI_RE = /\p{Extended_Pictographic}/u;

describe("no-emoji lint", () => {
  it("the renderer and the main process contain no emoji characters", () => {
    const violations: string[] = [];
    const pkgRoot = resolve(HERE, "..");

    for (const root of SCAN_ROOTS) {
      for (const file of collectSourceFiles(root)) {
        const lines = readFileSync(file, "utf8").split("\n");
        lines.forEach((line, i) => {
          if (EMOJI_RE.test(line)) {
            violations.push(`${relative(pkgRoot, file)}:${i + 1}: ${line.trim()}`);
          }
        });
      }
    }

    expect(
      violations,
      `Emoji found in source — replace with lucide-react icons:\n${violations.join("\n")}`,
    ).toEqual([]);
  });
});
