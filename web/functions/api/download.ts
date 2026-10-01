// /api/download — 302 to the right release artifact based on User-Agent.
//
// Why this exists: the hero's "Download for your device" button used to
// statically default to the macOS DMG and rely on JavaScript running before
// the user's click to swap the href to the right platform. Even on a Windows
// browser where the detection logic is correct, there's a race between page
// paint and the inline <script> at the bottom of the document — fast clickers
// got the DMG on Windows. Doing detection at the edge eliminates the race
// AND works with JS disabled, in older browsers, in private/strict modes,
// and with whatever extensions strip inline scripts.
//
// The `v` query param is the release tag baked by Astro at build time
// (e.g. "v0.23"). When the website rebuilds for the next version, the param
// updates and this function returns the new asset names without redeploying
// the function. If `v` is missing we land on the GitHub releases index — the
// worst case is one extra click for the user.
//
// Caching: the redirect itself is cheap so we keep the function dynamic
// (no caches.default put). Each request reads User-Agent fresh.

interface Env {}

const BASE = "https://github.com/zhitongblog/unterm/releases";

/// What the browser's client hints say about the CPU, when it sent them.
/// `"arm"` for Windows on ARM: Chromium's User-Agent there is frozen at
/// `Win64; x64`, so without the hint every ARM laptop got the x64 MSI.
function hintedArch(req: Request): string | null {
  const arch = req.headers.get("sec-ch-ua-arch");
  return arch ? arch.replace(/"/g, "").toLowerCase() : null;
}

function targetForUA(ua: string, v: string, arch: string | null = null): string {
  // Construct the filenames the way Page.astro does — keep them in sync
  // here OR the click 404s. (See web/src/components/Page.astro for the
  // canonical formulas — DMG / AppImage / win.zip use the tag verbatim,
  // MSI uses 3-segment SemVer with no `v` prefix because WiX requires it.)
  const tag = v;
  const bare = v.replace(/^v/, "");
  const semver = bare.split(".").length === 2 ? `${bare}.0` : bare;
  const latest = `${BASE}/latest/download`;

  // Order matters: iOS / iPadOS UAs contain "Mac"; match Apple mobile first.
  if (/iPhone|iPad|iPod/i.test(ua)) {
    // No iOS build; send mobile visitors to the release page so they can
    // share a link from a phone without seeing a desktop DMG hit Downloads.
    return `${BASE}/latest`;
  }
  if (/Windows|Win64|Win32|WOW64/i.test(ua)) {
    // Windows on ARM: the UA carries "ARM64" (Edge/Chrome on Snapdragon /
    // Surface) or a bare "ARM;". Serve the native arm64 MSI; everything else
    // gets x64 (which also runs on ARM via emulation, so it's a safe default).
    if (arch === "arm" || /ARM64|aarch64|ARM;/i.test(ua)) {
      return `${latest}/Unterm-${semver}-arm64.msi`;
    }
    return `${latest}/Unterm-${semver}-x64.msi`;
  }
  if (/Mac OS X|Macintosh/i.test(ua)) {
    return `${latest}/Unterm-macos-${tag}.dmg`;
  }
  if (/Linux|X11|Ubuntu|Debian|Fedora/i.test(ua)) {
    return `${latest}/Unterm-${tag}-x86_64.AppImage`;
  }
  // Unknown / bot / very old UA — land on the releases page.
  return `${BASE}/latest`;
}

export const onRequestGet: PagesFunction<Env> = (ctx) => {
  const url = new URL(ctx.request.url);
  const v = url.searchParams.get("v");
  if (!v) {
    return Response.redirect(`${BASE}/latest`, 302);
  }
  const ua = ctx.request.headers.get("user-agent") || "";
  const arch = hintedArch(ctx.request);
  const response = Response.redirect(targetForUA(ua, v, arch), 302);
  // Ask Chromium for the architecture. `Critical-CH` makes a browser that
  // did not send it retry this request once with it, before following the
  // redirect -- the only way the very first click can know it is on ARM.
  const headers = new Headers(response.headers);
  headers.set("Accept-CH", "Sec-CH-UA-Arch, Sec-CH-UA-Platform");
  if (/Windows/i.test(ua) && arch === null) {
    headers.set("Critical-CH", "Sec-CH-UA-Arch");
  }
  headers.set("Vary", "User-Agent, Sec-CH-UA-Arch");
  headers.set("Cache-Control", "no-store");
  return new Response(null, { status: 302, headers });
};
