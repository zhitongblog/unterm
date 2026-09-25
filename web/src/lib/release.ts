// Which release the site offers, and where its files are.
//
// Two ways to be wrong, in opposite directions, and both have shipped:
//
//   - Follow the newest changelog heading blindly, and a version bump alone
//     points every download link at files GitHub has not got. That was
//     2026-09-12: the site rebuilt within the minute and every download
//     404'd until the tag was cut.
//   - Require GitHub to confirm the release, and a build that cannot reach
//     GitHub advertises the *previous* version forever. That was
//     2026-09-14: Cloudflare's builders share an egress IP, the unauth
//     limit is 60/hr per IP, every request came back 403, and the site sat
//     on 0.71.6 for a day while 0.71.7 was published and downloadable.
//
// So the question has three answers. `stats.tags` is `null` when GitHub did
// not answer, and a list when it did:
//
//   list, contains it   -> published; use it
//   list, lacks it      -> bumped but not released yet; use the newest that
//                          is in the list
//   null (no answer)    -> unknown; trust the changelog

import type { Stats } from "./stats";
import * as changelog from "../../../CHANGELOG.md";

export interface ChangelogEntry {
  tag: string;
  date: string;
}

/** `## v0.71.6 — 2026-09-10` becomes { tag, date }, newest first. */
export const changelogEntries: ChangelogEntry[] = changelog
  .getHeadings()
  .filter((h) => h.depth === 2)
  .map((h) => {
    const [tag, date] = h.text.split("—").map((part) => part.trim());
    return { tag, date };
  });

export const dlBaseUrl = "https://github.com/zhitongblog/unterm/releases";

export function resolveRelease(stats: Stats) {
  const published = stats.tags === null ? null : new Set(stats.tags);
  const entry =
    (published === null
      ? changelogEntries[0]
      : changelogEntries.find((e) => published.has(e.tag))) ??
    changelogEntries.find((e) => e.tag === stats.release) ??
    changelogEntries[0] ?? { tag: stats.release, date: "" };
  const tag = entry.tag;
  // MSI names carry three-part SemVer without the `v` (WiX insists on it).
  const semver = (() => {
    const r = tag.replace(/^v/, "");
    return r.split(".").length === 2 ? `${r}.0` : r;
  })();
  const latest = `${dlBaseUrl}/latest/download`;
  return {
    tag,
    date: entry.date,
    urls: {
      dmg: `${latest}/Unterm-macos-${tag}.dmg`,
      deb: `${latest}/unterm-${tag}.deb`,
      debArm: `${latest}/unterm-${tag}.arm64.deb`,
      appimage: `${latest}/Unterm-${tag}-x86_64.AppImage`,
      appimageArm: `${latest}/Unterm-${tag}-aarch64.AppImage`,
      msi: `${latest}/Unterm-${semver}-x64.msi`,
      msiArm: `${latest}/Unterm-${semver}-arm64.msi`,
      winZip: `${latest}/Unterm-windows-x64-${tag}.zip`,
      winZipArm: `${latest}/Unterm-windows-arm64-${tag}.zip`,
    },
  };
}
