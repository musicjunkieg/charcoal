#!/usr/bin/env node
// Git merge DRIVER for .chainlink/issues-export.json.
//
// Registered in .gitattributes as `merge=chainlink-union` and wired
// per-clone by scripts/install-hooks.sh:
//   git config merge.chainlink-union.name "chainlink issues-export union"
//   git config merge.chainlink-union.driver "node scripts/merge-chainlink-export.mjs %O %A %B"
//
// Git invokes it during a merge with three temp files — %O (base / common
// ancestor), %A (ours, ALSO the output path), %B (theirs) — and reads the
// result back from %A. Exit 0 = merged successfully, non-zero = leave a
// conflict for a human.
//
// Why a union instead of keep-ours: the export is the only git-visible
// carrier of chainlink issue records (issues.db is untracked and per-
// worktree), so two branches can hold disjoint records. keep-ours would
// silently drop one side. This driver takes the UNION of both sides by
// issue id — nothing is lost — so a plain `git pull` auto-resolves the
// file correctly with no manual step. On the same id in both sides, the
// record with the later updated_at wins. The next pre-commit regenerates
// the export from the local db; the union here keeps the merge itself
// lossless in the meantime.
//
// Deletions are deliberately NOT propagated (union errs toward keeping a
// record): losing a real record is worse than resurrecting a deleted one,
// which the next `chainlink close`/regeneration corrects anyway.

import { readFileSync, writeFileSync } from "node:fs";

const [, , basePath, oursPath, theirsPath] = process.argv;

if (!oursPath) {
  console.error(
    "merge-chainlink-export: missing %A (ours/output) path argument",
  );
  process.exit(2);
}

/** Read a stage file into {version, exported_at, issues[]}; missing/empty = empty export. */
function readExport(path, label) {
  if (!path) return { version: 1, exported_at: null, issues: [] };
  let raw;
  try {
    raw = readFileSync(path, "utf8");
  } catch {
    return { version: 1, exported_at: null, issues: [] }; // absent stage (e.g. added on one side)
  }
  if (raw.trim() === "") return { version: 1, exported_at: null, issues: [] };
  try {
    const d = JSON.parse(raw);
    return {
      version: typeof d.version === "number" ? d.version : 1,
      exported_at: typeof d.exported_at === "string" ? d.exported_at : null,
      issues: Array.isArray(d.issues) ? d.issues : [],
    };
  } catch (e) {
    // Unparseable JSON is not something to silently paper over — leave the
    // conflict for a human rather than emit a plausible-but-wrong union.
    console.error(
      `merge-chainlink-export: ${label} (${path}) is not valid JSON — leaving conflict.`,
      e.message,
    );
    process.exit(1);
  }
}

const ours = readExport(oursPath, "ours");
const theirs = readExport(theirsPath, "theirs");
readExport(basePath, "base"); // validated but unused: union ignores the base by design

// Union by id; later updated_at wins on collision. Preserve each record's
// own key order (we keep the parsed object as-is).
const byId = new Map();
const consider = (issue) => {
  if (!issue || typeof issue.id === "undefined") return;
  const prev = byId.get(issue.id);
  if (!prev) {
    byId.set(issue.id, issue);
    return;
  }
  const a = typeof prev.updated_at === "string" ? prev.updated_at : "";
  const b = typeof issue.updated_at === "string" ? issue.updated_at : "";
  if (b >= a) byId.set(issue.id, issue);
};
for (const i of ours.issues) consider(i);
for (const i of theirs.issues) consider(i);

// chainlink exports issues by id descending; match it so the file doesn't thrash.
const issues = [...byId.values()].sort((x, y) => Number(y.id) - Number(x.id));

const result = {
  version: Math.max(ours.version ?? 1, theirs.version ?? 1),
  // later of the two export timestamps (nulls sort first)
  exported_at:
    (theirs.exported_at ?? "") > (ours.exported_at ?? "")
      ? theirs.exported_at
      : ours.exported_at,
  issues,
};

// 2-space indent, no trailing newline — matches `chainlink export`.
writeFileSync(oursPath, JSON.stringify(result, null, 2));
process.exit(0);
