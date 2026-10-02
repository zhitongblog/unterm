// Edge proxy at /api/ambassador-posts: the posts people submitted through
// .github/ISSUE_TEMPLATE/ambassador.yml, for the wall on /ambassador.
//
// Moderation is GitHub's own: an open issue labelled `ambassador-post` is
// listed, closing it (spam, dead link, broken rules) takes it off the page.
// Same edge-cache shape as /api/stats so GitHub is asked at most once per
// region every few minutes, and a failure is cached only briefly.

interface Env {
  GITHUB_TOKEN?: string;
}

const REPO = "zhitongblog/unterm";
const LABEL = "ambassador-post";
const SUCCESS_MAX_AGE = 300; // 5 min
const FAILURE_MAX_AGE = 30; // 30 s
const MAX_POSTS = 60;

interface Post {
  url: string;
  title: string;
  host: string;
  author: string;
  author_url: string;
  avatar: string;
  issue: string;
  created_at: string;
}

interface Issue {
  html_url: string;
  title: string;
  body: string | null;
  created_at: string;
  pull_request?: unknown;
  user: { login: string; html_url: string; avatar_url: string } | null;
}

export const onRequestGet: PagesFunction<Env> = async (ctx) => {
  const cacheKey = new Request("https://unterm.app/__ambassador_posts_v1", { method: "GET" });
  const cache = caches.default;
  const cached = await cache.match(cacheKey);
  if (cached) return cached;

  const res = await buildResponse(ctx.env);
  ctx.waitUntil(cache.put(cacheKey, res.clone()));
  return res;
};

async function buildResponse(env: Env): Promise<Response> {
  const headers: Record<string, string> = {
    "User-Agent": "unterm-site-edge",
    Accept: "application/vnd.github+json",
  };
  if (env.GITHUB_TOKEN) headers.Authorization = `Bearer ${env.GITHUB_TOKEN}`;

  try {
    const r = await fetch(
      `https://api.github.com/repos/${REPO}/issues?labels=${LABEL}&state=open&per_page=100&sort=created&direction=desc`,
      { headers, cf: { cacheTtl: SUCCESS_MAX_AGE, cacheEverything: true } },
    );
    if (!r.ok) {
      console.warn(`[ambassador-posts] github non-ok: ${r.status}`);
      return json({ posts: null }, FAILURE_MAX_AGE);
    }
    const issues = (await r.json()) as Issue[];
    const posts = issues
      .filter((i) => !i.pull_request && i.user)
      .map(toPost)
      .filter((p): p is Post => p !== null)
      .slice(0, MAX_POSTS);
    return json({ posts }, SUCCESS_MAX_AGE);
  } catch (e) {
    console.warn(`[ambassador-posts] fetch failed: ${e}`);
    return json({ posts: null }, FAILURE_MAX_AGE);
  }
}

/** An issue-form body is `### <label>\n\n<answer>` sections. */
export function sections(body: string): Record<string, string> {
  const out: Record<string, string> = {};
  const parts = body.split(/^###\s+/m).slice(1);
  for (const part of parts) {
    const nl = part.indexOf("\n");
    if (nl < 0) continue;
    const label = part.slice(0, nl).trim().toLowerCase();
    const value = part.slice(nl + 1).trim();
    out[label] = value === "_No response_" ? "" : value;
  }
  return out;
}

function pick(s: Record<string, string>, prefix: string): string {
  const key = Object.keys(s).find((k) => k.startsWith(prefix));
  return key ? s[key] : "";
}

export function toPost(i: Issue): Post | null {
  const body = i.body ?? "";
  const s = sections(body);
  const raw = pick(s, "link") || body;
  const m = raw.match(/https?:\/\/[^\s<>()"'`\]]+/);
  if (!m) return null;
  let url: URL;
  try {
    url = new URL(m[0]);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return null;
  const title =
    pick(s, "title").split("\n")[0].trim() ||
    i.title.replace(/^\[(post|ambassador)\]\s*/i, "").trim() ||
    url.hostname;
  return {
    url: url.toString(),
    title: title.slice(0, 160),
    host: url.hostname.replace(/^www\./, ""),
    author: i.user!.login,
    author_url: i.user!.html_url,
    avatar: i.user!.avatar_url,
    issue: i.html_url,
    created_at: i.created_at,
  };
}

function json(body: unknown, maxAge: number): Response {
  return new Response(JSON.stringify(body), {
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": `public, max-age=${maxAge}`,
    },
  });
}
