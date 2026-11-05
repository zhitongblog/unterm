// Generates public/llms-full.txt from the site's own sources so the file an
// answer engine reads can never drift from the docs a person reads.
//
//   llms.txt (summary)  +  competitor matrix (src/data/competitors.ts)
//   +  every page under src/pages/docs and src/pages/blog (frontmatter stripped)
//
// Run automatically before `astro build` (see package.json). Nothing here is
// hand-written product copy: edit the docs, not this output.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(join(root, p), "utf8");

// --- Competitor matrix -----------------------------------------------------
const en = JSON.parse(read("src/i18n/en-US.json"));
const ts = read("src/data/competitors.ts");
const parseValues = (block) =>
  Object.fromEntries([...block.matchAll(/"(cmp\.r\.[a-z0-9]+)":\s*"([^"]*)"/g)].map((m) => [m[1], m[2]]));
const rows = [...ts.matchAll(/\{ key: "(cmp\.r\.[a-z0-9]+)" \}/g)].map((m) => m[1]);
const unterm = parseValues(ts.match(/UNTERM_VALUES[^{]*\{([\s\S]*?)\n\};/)[1]);
const competitors = [...ts.matchAll(/slug: "([^"]+)",\s*name: "([^"]+)",[\s\S]*?values: \{([\s\S]*?)\n    \},/g)].map(
  (m) => ({ slug: m[1], name: m[2], values: parseValues(m[3]) }),
);
const header = ["Capability", "Unterm", ...competitors.map((c) => c.name)];
const table = [
  `| ${header.join(" | ")} |`,
  `| ${header.map(() => "---").join(" | ")} |`,
  ...rows.map((r) => `| ${[en[r] ?? r, unterm[r] ?? "", ...competitors.map((c) => c.values[r] ?? "")].join(" | ")} |`),
].join("\n");
const compareLinks = competitors.map((c) => `- Unterm vs ${c.name}: https://unterm.app/compare/${c.slug}`).join("\n");

// --- Docs and blog -----------------------------------------------------------
const pages = (dir, base) =>
  readdirSync(join(root, dir))
    .filter((f) => f.endsWith(".md"))
    .sort()
    .map((f) => {
      const src = read(join(dir, f));
      const fm = src.match(/^---\n([\s\S]*?)\n---\n/);
      const title = fm?.[1].match(/^title:\s*"?(.*?)"?\s*$/m)?.[1] ?? f;
      const body = fm ? src.slice(fm[0].length) : src;
      return `\n\n---\n\n# ${title}\n\nSource: https://unterm.app/${base}/${f.replace(/\.md$/, "")}\n\n${body.trim()}`;
    })
    .join("");

const out = `${read("public/llms.txt").trim()}

---

# Comparison matrix

Values as shown on https://unterm.app/compare (✓ yes, ✗ no, ~ partial).

${table}

${compareLinks}
${pages("src/pages/docs", "docs")}
${pages("src/pages/blog", "blog")}
`;

writeFileSync(join(root, "public/llms-full.txt"), out);
console.log(`llms-full.txt: ${(out.length / 1024).toFixed(0)} KB, ${competitors.length} competitors, ${rows.length} rows`);
