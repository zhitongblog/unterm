// Single source of truth for the /compare/<slug> competitor pages.
//
// Each competitor renders a two-lane "Unterm vs X" feature table plus honest
// prose (why teams pick Unterm, where the competitor is genuinely stronger, a
// one-line verdict). Table cell values are deliberately language-neutral
// tokens (✓ / ✗ / ~ / proper nouns like MIT, Apache-2.0, $0) so the matrix
// needs no per-locale translation — only the row LABELS (cmp.r.*, already in
// the i18n dicts) and the prose keys (cmp.c.<slug>.*) are localized.
//
// The Unterm-specific rows (mcp / agents / autowire / identity / instance /
// rec / capture / proxy) are ✗ for every competitor because they describe
// Unterm's own agent-control surface — no general-purpose terminal ships them.
// Where a competitor DOES have an adjacent capability (a control CLI, a
// built-in AI widget) we say so honestly rather than marking a bare ✗.

export interface CmpRow {
  /** i18n key for the row label, e.g. "cmp.r.mcp". */
  key: string;
}

// Row order = differentiators first, then table-stakes rendering/platform.
// Mirrors the homepage #compare table's row order (cmp.r.* keys already exist).
export const CMP_ROWS: CmpRow[] = [
  { key: "cmp.r.mcp" },
  { key: "cmp.r.cli" },
  { key: "cmp.r.agents" },
  { key: "cmp.r.autowire" },
  { key: "cmp.r.cockpit" },
  { key: "cmp.r.identity" },
  { key: "cmp.r.instance" },
  { key: "cmp.r.rec" },
  { key: "cmp.r.capture" },
  { key: "cmp.r.rightclick" },
  { key: "cmp.r.proxy" },
  { key: "cmp.r.local" },
  { key: "cmp.r.ai" },
  { key: "cmp.r.gpu" },
  { key: "cmp.r.cross" },
  { key: "cmp.r.oss" },
  { key: "cmp.r.i18n" },
  { key: "cmp.r.price" },
];

/** Unterm's column — same values as the homepage table's `unterm` lane. */
export const UNTERM_VALUES: Record<string, string> = {
  "cmp.r.mcp": "✓",
  "cmp.r.cli": "✓",
  "cmp.r.agents": "✓ ×9",
  "cmp.r.autowire": "✓",
  "cmp.r.cockpit": "✓",
  "cmp.r.identity": "✓",
  "cmp.r.instance": "✓",
  "cmp.r.rec": "✓ md",
  "cmp.r.capture": "✓",
  "cmp.r.rightclick": "✓ gesture",
  "cmp.r.proxy": "✓",
  "cmp.r.local": "✓",
  "cmp.r.ai": "✓ agent-driven",
  "cmp.r.gpu": "✓",
  "cmp.r.cross": "✓",
  "cmp.r.oss": "✓ MIT",
  "cmp.r.i18n": "✓ ×9",
  "cmp.r.price": "$0",
};

export interface Competitor {
  slug: string;
  /** Display name, used in H1 "Unterm vs {name}" and <title>. */
  name: string;
  /** Upstream product URL (rel=nofollow external link). */
  url: string;
  /** Short category chip shown under the name. */
  category: "upstream" | "ai-cloud" | "ai-oss" | "power" | "native" | "headless";
  /** Number of honest angle bullets that exist (cmp.c.<slug>.a1..aN). */
  angleCount: number;
  /** Feature-matrix values keyed by CMP_ROWS key. */
  values: Record<string, string>;
}

// Unterm-unique rows are ✗ for all; general rows use verified facts.
export const COMPETITORS: Competitor[] = [
  {
    slug: "wezterm",
    name: "WezTerm",
    url: "https://wezfurlong.org/wezterm/",
    category: "upstream",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗",
      "cmp.r.cli": "~ pane cli", // `wezterm cli` controls panes, not an agent domain
      "cmp.r.agents": "✗",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "✗",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "✗",
      "cmp.r.rec": "✗",
      "cmp.r.capture": "✗",
      "cmp.r.rightclick": "~ config",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✓",
      "cmp.r.ai": "✗",
      "cmp.r.gpu": "✓",
      "cmp.r.cross": "✓",
      "cmp.r.oss": "✓ MIT",
      "cmp.r.i18n": "en",
      "cmp.r.price": "$0",
    },
  },
  {
    slug: "warp",
    name: "Warp",
    url: "https://www.warp.dev/",
    category: "ai-cloud",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗",
      "cmp.r.cli": "✗",
      "cmp.r.agents": "✗",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "~ cloud",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "✗",
      "cmp.r.rec": "~",
      "cmp.r.capture": "✗",
      "cmp.r.rightclick": "menu",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✗ (login)",
      "cmp.r.ai": "✓ cloud",
      "cmp.r.gpu": "✓",
      "cmp.r.cross": "✓",
      "cmp.r.oss": "✗",
      "cmp.r.i18n": "en",
      "cmp.r.price": "freemium",
    },
  },
  {
    slug: "wave-terminal",
    name: "Wave Terminal",
    url: "https://www.waveterm.dev/",
    category: "ai-oss",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗",
      "cmp.r.cli": "~ wsh", // `wsh` controls Wave blocks
      "cmp.r.agents": "✗",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "✗",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "✗",
      "cmp.r.rec": "✗",
      "cmp.r.capture": "~ preview",
      "cmp.r.rightclick": "menu",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✓",
      "cmp.r.ai": "✓ widget (BYO)",
      "cmp.r.gpu": "✓",
      "cmp.r.cross": "✓",
      "cmp.r.oss": "✓ Apache-2.0",
      "cmp.r.i18n": "en",
      "cmp.r.price": "$0",
    },
  },
  {
    slug: "terax",
    name: "Terax",
    url: "https://terax.app/",
    category: "ai-oss",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗",
      "cmp.r.cli": "✗",
      "cmp.r.agents": "✓ built-in",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "✗",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "✗",
      "cmp.r.rec": "✗",
      "cmp.r.capture": "~ preview",
      "cmp.r.rightclick": "—",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✓",
      "cmp.r.ai": "✓ built-in (BYO)",
      "cmp.r.gpu": "✓ WebGL",
      "cmp.r.cross": "✓ +Android",
      "cmp.r.oss": "✓",
      "cmp.r.i18n": "en",
      "cmp.r.price": "$0",
    },
  },
  {
    slug: "rmux",
    name: "rmux",
    url: "https://rmux.io/",
    category: "headless",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗ (SDK only)",
      "cmp.r.cli": "✓ tmux-compat",
      "cmp.r.agents": "✗",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "✗",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "~ sessions",
      "cmp.r.rec": "~ snapshots",
      "cmp.r.capture": "✗",
      "cmp.r.rightclick": "—",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✓",
      "cmp.r.ai": "✗",
      "cmp.r.gpu": "— (no GUI)",
      "cmp.r.cross": "✓",
      "cmp.r.oss": "✓ MIT/Apache",
      "cmp.r.i18n": "en",
      "cmp.r.price": "$0",
    },
  },
  {
    slug: "iterm2",
    name: "iTerm2",
    url: "https://iterm2.com/",
    category: "power",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗",
      "cmp.r.cli": "Python API",
      "cmp.r.agents": "✗",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "✗",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "✗",
      "cmp.r.rec": "✓",
      "cmp.r.capture": "~",
      "cmp.r.rightclick": "menu",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✓",
      "cmp.r.ai": "plugin",
      "cmp.r.gpu": "✓",
      "cmp.r.cross": "macOS",
      "cmp.r.oss": "✓ GPL",
      "cmp.r.i18n": "en",
      "cmp.r.price": "$0",
    },
  },
  {
    slug: "ghostty",
    name: "Ghostty",
    url: "https://ghostty.org/",
    category: "power",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗",
      "cmp.r.cli": "✗",
      "cmp.r.agents": "✗",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "✗",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "✗",
      "cmp.r.rec": "✗",
      "cmp.r.capture": "✗",
      "cmp.r.rightclick": "menu",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✓",
      "cmp.r.ai": "✗",
      "cmp.r.gpu": "✓",
      "cmp.r.cross": "mac+Linux",
      "cmp.r.oss": "✓ MIT",
      "cmp.r.i18n": "en",
      "cmp.r.price": "$0",
    },
  },
  {
    slug: "windows-terminal",
    name: "Windows Terminal",
    url: "https://github.com/microsoft/terminal",
    category: "native",
    angleCount: 3,
    values: {
      "cmp.r.mcp": "✗",
      "cmp.r.cli": "~ wt.exe",
      "cmp.r.agents": "✗",
      "cmp.r.autowire": "✗",
      "cmp.r.cockpit": "✗",
      "cmp.r.identity": "✗",
      "cmp.r.instance": "✗",
      "cmp.r.rec": "✗",
      "cmp.r.capture": "✗",
      "cmp.r.rightclick": "paste",
      "cmp.r.proxy": "✗",
      "cmp.r.local": "✓",
      "cmp.r.ai": "✗",
      "cmp.r.gpu": "✓ DX11/12",
      "cmp.r.cross": "Windows",
      "cmp.r.oss": "✓ MIT",
      "cmp.r.i18n": "✓",
      "cmp.r.price": "$0",
    },
  },
];

export function getCompetitor(slug: string): Competitor | undefined {
  return COMPETITORS.find((c) => c.slug === slug);
}

const ORIGIN = "https://unterm.app";

/** Site-absolute path for a compare page in a given locale prefix. */
export function comparePath(pathPrefix: string, slug: string): string {
  return `${pathPrefix}/compare/${slug}`;
}

/** hreflang alternates for one compare page across every locale. */
export function compareAlternates(
  slug: string,
  locales: { code: string; href: string }[],
): { code: string; href: string }[] {
  return locales.map((l) => {
    const prefix = l.href === "/" ? "" : l.href.replace(/\/$/, "");
    return { code: l.code, href: `${ORIGIN}${prefix}/compare/${slug}` };
  });
}

/** JSON-LD graph for a compare page: breadcrumb + the Unterm app entity. */
export function compareJsonLd(
  slug: string,
  name: string,
  lang: string,
  pathPrefix: string,
  title: string,
  description: string,
) {
  const url = `${ORIGIN}${pathPrefix}/compare/${slug}`;
  const home = `${ORIGIN}${pathPrefix || "/"}`;
  return [
    {
      "@context": "https://schema.org",
      "@type": "BreadcrumbList",
      itemListElement: [
        { "@type": "ListItem", position: 1, name: "Unterm", item: home },
        { "@type": "ListItem", position: 2, name: `Unterm vs ${name}`, item: url },
      ],
    },
    {
      "@context": "https://schema.org",
      "@type": "TechArticle",
      headline: title,
      description,
      inLanguage: lang,
      url,
      about: { "@type": "SoftwareApplication", name: "Unterm", applicationCategory: "DeveloperApplication", operatingSystem: "macOS, Linux, Windows" },
      publisher: { "@type": "Organization", name: "Unzoo", url: ORIGIN + "/" },
    },
  ];
}
