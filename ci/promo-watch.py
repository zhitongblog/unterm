#!/usr/bin/env python3
"""Look for people talking about Unterm, and report only what is new.

    python3 ci/promo-watch.py            # report what's new since last run
    python3 ci/promo-watch.py --all      # report everything found, state untouched
    python3 ci/promo-watch.py --dry-run  # report what's new, state untouched
    python3 ci/promo-watch.py --notify   # also raise a macOS notification if anything is new
    python3 ci/promo-watch.py --install  # run daily at 10:00 (macOS launchd), notify when new

Sources, each failing on its own without taking the others down:

  - posts shared through the ambassador form (issues labelled ambassador-post)
  - GitHub issues / PRs / discussions elsewhere that mention the repo or site
  - code (READMEs, awesome-lists) elsewhere that links the repo or site
  - Hacker News stories and comments linking the site or repo
  - V2EX topics that mention Unterm (via sov2ex)
  - where GitHub visitors came from (traffic referrers, last 14 days)
  - stars and repository views, as a delta since the last run

State lives in ~/.unterm/promo-watch/state.json and each run writes a dated
Markdown report next to it in reports/. Needs `gh` logged in with push access
to the repository (the traffic API requires it).

--install copies this file into that directory and points a launchd agent at
the copy, not at the checkout: the checkout may sit on an external volume,
which macOS will not let a background job read without asking. Run it again
after changing the script.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request
from pathlib import Path

REPO = "zhitongblog/unterm"
OWNER = REPO.split("/")[0]
HOME = Path(os.environ.get("UNTERM_PROMO_WATCH_DIR", Path.home() / ".unterm" / "promo-watch"))
STATE = HOME / "state.json"
REPORTS = HOME / "reports"
UA = "unterm-promo-watch/1.0 (+https://unterm.app)"
LABEL = "app.unterm.promo-watch"


def gh(*args: str) -> object:
    out = subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout
    return json.loads(out) if out.strip() else None


def http_json(url: str) -> object:
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.load(r)


# ---------------------------------------------------------------- sources --
# Each returns a list of items {id, title, url, who, when, note?}; `id` is
# what decides "seen before".


def ambassador_posts(me: str) -> list[dict]:
    issues = gh("issue", "list", "-R", REPO, "--label", "ambassador-post", "--state", "all",
                "--limit", "100", "--json", "number,title,url,author,createdAt,state")
    return [{
        "id": f"amb:{i['number']}",
        "title": i["title"].removeprefix("[Post] ").strip() or f"#{i['number']}",
        "url": i["url"],
        "who": i["author"]["login"],
        "when": i["createdAt"][:10],
        "note": "closed" if i["state"] != "OPEN" else "",
    } for i in issues if i["author"]["login"] != me]


def github_mentions(me: str) -> list[dict]:
    items: dict[str, dict] = {}
    for q in (f'"{REPO}" -repo:{REPO}', f'"unterm.app" -repo:{REPO}'):
        res = gh("api", "-X", "GET", "search/issues", "-f", f"q={q}", "-f", "per_page=50",
                 "-f", "sort=created", "-f", "order=desc")
        for i in res["items"]:
            if i["user"]["login"] in (me, OWNER):
                continue
            items[i["html_url"]] = {
                "id": f"gh:{i['html_url']}",
                "title": i["title"],
                "url": i["html_url"],
                "who": i["user"]["login"],
                "when": i["created_at"][:10],
            }
    return list(items.values())


def code_mentions(me: str) -> list[dict]:
    items: dict[str, dict] = {}
    for q in (f'"{REPO}"', '"unterm.app"'):
        res = gh("api", "-X", "GET", "search/code", "-f", f"q={q}", "-f", "per_page=50",
                 "-H", "Accept: application/vnd.github.text-match+json")
        # Whole words only: "unterm.app" is also the start of "unterm.append(".
        needle = re.compile(r"(?<![\w.-])" + re.escape(q.strip('"').lower()) + r"(?![\w-])")
        for i in res["items"]:
            repo = i["repository"]["full_name"]
            if repo.split("/")[0] in (me, OWNER):
                continue
            # Code search ignores punctuation, so "unterm.app" also finds
            # "unterm app"; keep only files whose matched text has it verbatim.
            fragments = " ".join(m.get("fragment", "") for m in i.get("text_matches", [])).lower()
            if not needle.search(fragments):
                continue
            items[repo] = {
                "id": f"code:{repo}",
                "title": f"{repo} — {i['path']}",
                "url": i["html_url"],
                "who": repo.split("/")[0],
                "when": "",
            }
    return list(items.values())


def hacker_news(_me: str) -> list[dict]:
    items: dict[str, dict] = {}
    for q in ("unterm.app", REPO):
        url = "https://hn.algolia.com/api/v1/search_by_date?" + urllib.parse.urlencode(
            {"query": q, "tags": "(story,comment)", "hitsPerPage": 50})
        for h in http_json(url)["hits"]:
            oid = h["objectID"]
            title = h.get("title") or h.get("story_title") or (h.get("comment_text") or "")[:80]
            items[oid] = {
                "id": f"hn:{oid}",
                "title": title,
                "url": f"https://news.ycombinator.com/item?id={oid}",
                "who": h.get("author", ""),
                "when": (h.get("created_at") or "")[:10],
            }
    return list(items.values())


def v2ex(_me: str) -> list[dict]:
    url = "https://www.sov2ex.com/api/search?" + urllib.parse.urlencode(
        {"q": "unterm", "size": 50, "sort": "created"})
    out = []
    for h in http_json(url).get("hits", []):
        s = h["_source"]
        text = f"{s.get('title', '')} {s.get('content', '')}".lower()
        # "unterm" alone also matches other words; require it next to what it is.
        if "unterm" not in text or not any(k in text for k in ("终端", "terminal", "agent", "claude", "mcp")):
            continue
        out.append({
            "id": f"v2ex:{s['id']}",
            "title": s.get("title", ""),
            "url": f"https://www.v2ex.com/t/{s['id']}",
            "who": s.get("member", ""),
            "when": (s.get("created") or "")[:10],
        })
    return out


def referrers(_me: str) -> list[dict]:
    rows = gh("api", f"repos/{REPO}/traffic/popular/referrers")
    return [{
        "id": f"ref:{r['referrer']}",
        "title": r["referrer"],
        "url": "",
        "who": "",
        "when": "",
        "note": f"{r['count']} views / {r['uniques']} visitors in 14 days",
    } for r in rows]


SOURCES = [
    ("Posts shared through the ambassador form", ambassador_posts),
    ("Mentioned in GitHub issues / PRs elsewhere", github_mentions),
    ("Linked from code / READMEs elsewhere", code_mentions),
    ("Hacker News", hacker_news),
    ("V2EX", v2ex),
    ("New places GitHub visitors came from", referrers),
]


def numbers() -> dict:
    repo = gh("api", f"repos/{REPO}")
    views = gh("api", f"repos/{REPO}/traffic/views")
    return {"stars": repo["stargazers_count"], "forks": repo["forks_count"],
            "views_14d": views["count"], "visitors_14d": views["uniques"]}


def install() -> int:
    import plistlib
    import shutil

    HOME.mkdir(parents=True, exist_ok=True)
    script = HOME / "promo-watch.py"
    shutil.copy2(__file__, script)
    plist = Path.home() / "Library" / "LaunchAgents" / f"{LABEL}.plist"
    plist.parent.mkdir(parents=True, exist_ok=True)
    gh_dir = str(Path(shutil.which("gh") or "/opt/homebrew/bin/gh").parent)
    with plist.open("wb") as f:
        plistlib.dump({
            "Label": LABEL,
            "ProgramArguments": [sys.executable, str(script), "--notify"],
            "EnvironmentVariables": {"PATH": f"{gh_dir}:/usr/local/bin:/usr/bin:/bin", "HOME": str(Path.home())},
            "StartCalendarInterval": {"Hour": 10, "Minute": 0},
            "StandardOutPath": str(HOME / "launchd.log"),
            "StandardErrorPath": str(HOME / "launchd.log"),
        }, f)
    domain = f"gui/{os.getuid()}"
    subprocess.run(["launchctl", "bootout", f"{domain}/{LABEL}"], capture_output=True)
    subprocess.run(["launchctl", "bootstrap", domain, str(plist)], check=True)
    print(f"installed {plist} -> {script}, daily at 10:00")
    return 0


# ----------------------------------------------------------------- report --


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--all", action="store_true", help="report everything found; do not update state")
    ap.add_argument("--dry-run", action="store_true", help="do not update state")
    ap.add_argument("--notify", action="store_true", help="macOS notification when something is new")
    ap.add_argument("--install", action="store_true", help="run daily at 10:00 via launchd")
    args = ap.parse_args()
    if args.install:
        return install()

    state = json.loads(STATE.read_text()) if STATE.exists() else {"seen": [], "numbers": {}}
    seen = set(state["seen"])
    first_run = not STATE.exists()
    me = gh("api", "user")["login"]

    today = dt.date.today().isoformat()
    lines = [f"# Unterm promotion watch — {today}", ""]
    new_count = 0
    errors = []

    try:
        now = numbers()
        before = state.get("numbers") or {}
        def delta(k):
            return f" ({now[k] - before[k]:+d})" if k in before else ""
        lines += [f"**Stars** {now['stars']}{delta('stars')} · **forks** {now['forks']}{delta('forks')} · "
                  f"**repo views, last 14 days** {now['views_14d']} by {now['visitors_14d']} visitors", ""]
    except Exception as e:  # noqa: BLE001 — one source failing must not stop the rest
        now = state.get("numbers") or {}
        errors.append(f"numbers: {e}")

    for title, fn in SOURCES:
        try:
            items = fn(me)
        except Exception as e:  # noqa: BLE001
            errors.append(f"{title}: {e}")
            continue
        fresh = items if args.all else [i for i in items if i["id"] not in seen]
        seen.update(i["id"] for i in items)
        if not fresh:
            continue
        new_count += len(fresh)
        lines += [f"## {title} ({len(fresh)})", ""]
        for i in fresh:
            link = f"[{i['title']}]({i['url']})" if i["url"] else i["title"]
            meta = " · ".join(x for x in (i["who"], i["when"], i.get("note", "")) if x)
            lines.append(f"- {link}" + (f" — {meta}" if meta else ""))
        lines.append("")

    if new_count == 0:
        lines += ["Nothing new since the last run.", ""]
    if errors:
        lines += ["## Sources that failed this time", ""] + [f"- {e}" for e in errors] + [""]
    if first_run and not args.all:
        lines += ["_First run: everything above is new because nothing has been seen yet._", ""]

    report = "\n".join(lines)
    print(report)

    if not (args.all or args.dry_run):
        HOME.mkdir(parents=True, exist_ok=True)
        REPORTS.mkdir(parents=True, exist_ok=True)
        (REPORTS / f"{today}.md").write_text(report)
        STATE.write_text(json.dumps({"seen": sorted(seen), "numbers": now,
                                     "last_run": dt.datetime.now().isoformat(timespec="seconds")}, indent=2))

    if args.notify and new_count:
        msg = f"{new_count} new mention(s) of Unterm — see {REPORTS / (today + '.md')}"
        subprocess.run(["osascript", "-e", f'display notification "{msg}" with title "Unterm promotion watch"'],
                       check=False)
    return 1 if errors and new_count == 0 and len(errors) == len(SOURCES) else 0


if __name__ == "__main__":
    sys.exit(main())
