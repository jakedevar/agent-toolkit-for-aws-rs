import { cp, mkdir, rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const skillRoot = path.resolve(packageRoot, "../..");
const outputRoot = path.join(packageRoot, "skills", "agents-pay");
// The admin CLI is the shipped bin/toolkit binary, so the bundled skill
// carries its instructions only (R-PKG-6).
const includedEntries = ["SKILL.md", "references"];

await rm(outputRoot, { recursive: true, force: true });
await mkdir(outputRoot, { recursive: true });

for (const entry of includedEntries) {
  await cp(path.join(skillRoot, entry), path.join(outputRoot, entry), {
    recursive: true,
  });
}
