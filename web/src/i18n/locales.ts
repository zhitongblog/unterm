// Locale registry shared by Base.astro (language picker) and the page entries
// (hreflang alternates). Order shown here is the order shown in the picker.

export interface LocaleEntry {
  code: string;
  name: string;
  /** Path on this site for that locale's home page (no trailing slash). */
  href: string;
}

export const LOCALES: LocaleEntry[] = [
  { code: "en-US", name: "English",   href: "/" },
  { code: "zh-CN", name: "简体中文",  href: "/zh-CN/" },
  { code: "zh-TW", name: "繁體中文",  href: "/zh-TW/" },
  { code: "ja-JP", name: "日本語",    href: "/ja-JP/" },
  { code: "ko-KR", name: "한국어",    href: "/ko-KR/" },
  { code: "de-DE", name: "Deutsch",   href: "/de-DE/" },
  { code: "fr-FR", name: "Français",  href: "/fr-FR/" },
  { code: "it-IT", name: "Italiano",  href: "/it-IT/" },
  { code: "hi-IN", name: "हिन्दी",     href: "/hi-IN/" },
];

/** hreflang link tags, absolute URLs. */
export function alternates() {
  const origin = "https://unterm.app";
  return LOCALES.map((l) => ({
    code: l.code,
    href: origin + l.href,
  }));
}

/** hreflang link tags for a sub-page, e.g. "/guide" -> /guide, /zh-CN/guide, … */
export function alternatesFor(path: string) {
  const origin = "https://unterm.app";
  return LOCALES.map((l) => ({
    code: l.code,
    href: origin + (l.href === "/" ? "" : l.href.replace(/\/$/, "")) + path,
  }));
}
