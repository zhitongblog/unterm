// Dynamic build-time sitemap.xml covering all 16 locales, agent landing pages,
// competitor comparison pages, docs, blog posts, and changelog — with full
// xhtml:link hreflang alternates so Google, Bing, and AI crawlers index every
// localized page cleanly.

import type { APIRoute } from "astro";
import { readdirSync } from "node:fs";
import { join } from "node:path";
import { LOCALES } from "../i18n/locales";
import { COMPETITORS } from "../data/competitors";
import { AGENT_LANDINGS } from "../data/agents";

const ORIGIN = "https://unterm.app";
const TODAY = new Date().toISOString().slice(0, 10);

function escapeXml(s: string): string {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function localeUrl(localeHref: string, subPath: string): string {
  if (subPath === "/") return `${ORIGIN}${localeHref}`;
  const prefix = localeHref === "/" ? "" : localeHref.replace(/\/$/, "");
  return `${ORIGIN}${prefix}${subPath}`;
}

function buildLocalizedEntries(
  subPath: string,
  priority: string,
  changefreq: string,
): string[] {
  const xDefault = localeUrl("/", subPath);
  const altLinks = LOCALES.map(
    (l) =>
      `    <xhtml:link rel="alternate" hreflang="${l.code}" href="${escapeXml(localeUrl(l.href, subPath))}" />`,
  );
  altLinks.push(
    `    <xhtml:link rel="alternate" hreflang="x-default" href="${escapeXml(xDefault)}" />`,
  );

  return LOCALES.map((l) => {
    const loc = localeUrl(l.href, subPath);
    return [
      "  <url>",
      `    <loc>${escapeXml(loc)}</loc>`,
      `    <lastmod>${TODAY}</lastmod>`,
      `    <changefreq>${changefreq}</changefreq>`,
      `    <priority>${l.code === "en-US" ? priority : (Number(priority) - 0.1).toFixed(1)}</priority>`,
      ...altLinks,
      "  </url>",
    ].join("\n");
  });
}

function buildStaticEntry(path: string, priority: string, changefreq: string): string {
  return [
    "  <url>",
    `    <loc>${escapeXml(`${ORIGIN}${path}`)}</loc>`,
    `    <lastmod>${TODAY}</lastmod>`,
    `    <changefreq>${changefreq}</changefreq>`,
    `    <priority>${priority}</priority>`,
    "  </url>",
  ].join("\n");
}

function listMarkdownSlugs(relDir: string): string[] {
  try {
    return readdirSync(join(process.cwd(), relDir))
      .filter((f) => f.endsWith(".md"))
      .map((f) => f.replace(/\.md$/, ""))
      .sort();
  } catch {
    return [];
  }
}

export const GET: APIRoute = () => {
  const urls: string[] = [];

  // Core localized pages across all 16 languages
  urls.push(...buildLocalizedEntries("/", "1.0", "daily"));
  urls.push(...buildLocalizedEntries("/features", "0.9", "weekly"));
  urls.push(...buildLocalizedEntries("/guide", "0.8", "weekly"));
  urls.push(...buildLocalizedEntries("/ambassador", "0.7", "monthly"));

  // High-intent AI Coding CLI landing pages across all 16 languages
  for (const agent of AGENT_LANDINGS) {
    urls.push(...buildLocalizedEntries(`/for/${agent.slug}`, "0.9", "weekly"));
  }

  // Competitor comparison pages across all 16 languages
  for (const c of COMPETITORS) {
    urls.push(...buildLocalizedEntries(`/compare/${c.slug}`, "0.8", "weekly"));
  }

  // Changelog, Docs & Blog
  urls.push(buildStaticEntry("/changelog", "0.8", "weekly"));
  for (const slug of listMarkdownSlugs("src/pages/docs")) {
    urls.push(buildStaticEntry(`/docs/${slug}`, "0.7", "weekly"));
  }
  for (const slug of listMarkdownSlugs("src/pages/blog")) {
    urls.push(buildStaticEntry(`/blog/${slug}`, "0.7", "monthly"));
  }

  const xml = [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">',
    ...urls,
    "</urlset>",
    "",
  ].join("\n");

  return new Response(xml, {
    headers: { "Content-Type": "application/xml; charset=utf-8" },
  });
};
