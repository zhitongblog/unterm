// Build-time fetch of repo stars + total downloads from the GitHub API.
//
// Called from the Astro page frontmatter, so the numbers are baked into
// every static HTML that we ship — no client-side API call, no CORS, no
// rate-limit dance, no flicker on first paint. Numbers refresh on every
// deploy; pushing a commit triggers a rebuild on Cloudflare Pages.
//
// We memoize across calls within a single build so all 9 locale pages
// share one fetch round-trip rather than hammering GitHub nine times.

const REPO = "zhitongblog/unterm";
const HEADERS: Record<string, string> = {
  "User-Agent": "unterm-site-build",
  Accept: "application/vnd.github+json",
  // Optional: drop in a token via env to lift the unauth rate limit. The
  // public unauth limit (60/hr/IP) is plenty for Pages builds, but local
  // `pnpm build` loops can hit it.
  ...(process.env.GITHUB_TOKEN
    ? { Authorization: `Bearer ${process.env.GITHUB_TOKEN}` }
    : {}),
};

export interface Stats {
  /** `null` means "no trustworthy answer yet" — render an em dash, not 0.
   *  Distinguishing null vs 0 is load-bearing: a transient GitHub 503 must
   *  not pin "⭐ 0 stars" onto the homepage for the next 5 minutes. */
  stars: number | null;
  downloads: number | null;
  /** Latest tag, e.g. "v0.17". Used for download links so we don't have
   *  to update the hero CTA every time we cut a release. */
  release: string;
}

// Fallback used when the build-time fetch fails entirely. We give the
// release a sane default because the hero CTA must point _somewhere_,
// but the numbers stay null so the chips render as em dashes and let
// the client-side refresh fill them in if it can.
//
// IMPORTANT: this string must match the actual latest git tag and asset
// filenames. The homepage builds direct links such as
// `releases/latest/download/Unterm-macos-${stats.release}.dmg`; if this
// fallback drifts from the published asset names, users hit a 404 when the
// GitHub API fetch fails at build time.
const FALLBACK: Stats = { stars: null, downloads: null, release: "v0.71.5" };

let cache: Promise<Stats> | null = null;

export function fetchStats(): Promise<Stats> {
  if (!cache) cache = doFetch();
  return cache;
}

async function doFetch(): Promise<Stats> {
  try {
    const [repoRes, releasesRes] = await Promise.all([
      fetch(`https://api.github.com/repos/${REPO}`, { headers: HEADERS }),
      fetch(`https://api.github.com/repos/${REPO}/releases?per_page=100`, {
        headers: HEADERS,
      }),
    ]);

    if (!repoRes.ok || !releasesRes.ok) {
      console.warn(
        `[stats] GitHub API non-OK: repo=${repoRes.status} releases=${releasesRes.status}`,
      );
      return FALLBACK;
    }

    const repoJson: any = await repoRes.json();
    const releasesJson: any[] = await releasesRes.json();

    const downloads = Array.isArray(releasesJson)
      ? releasesJson.reduce(
          (sum, r) =>
            sum +
            (Array.isArray(r.assets)
              ? r.assets.reduce(
                  (s: number, a: any) => s + (a.download_count ?? 0),
                  0,
                )
              : 0),
          0,
        )
      : 0;

    return {
      stars: repoJson.stargazers_count ?? 0,
      downloads,
      release: releasesJson?.[0]?.tag_name ?? FALLBACK.release,
    };
  } catch (err) {
    console.warn("[stats] failed to fetch GitHub stats:", err);
    return FALLBACK;
  }
}

/**
 * 1234 -> "1.2k", 12345 -> "12k", 1_234_567 -> "1.2M".
 * Anything under 1000 is shown raw. Trailing ".0" is stripped so we get
 * "5k" not "5.0k".
 *
 * `null` renders as an em dash so the chip layout doesn't collapse and
 * the user can tell "we don't know yet" apart from "the answer is zero".
 */
export function formatCount(n: number | null): string {
  if (n === null || !Number.isFinite(n)) return "—";
  if (n < 1000) return String(n);
  if (n < 10_000) return (n / 1000).toFixed(1).replace(/\.0$/, "") + "k";
  if (n < 1_000_000) return Math.round(n / 1000) + "k";
  return (n / 1_000_000).toFixed(1).replace(/\.0$/, "") + "M";
}
