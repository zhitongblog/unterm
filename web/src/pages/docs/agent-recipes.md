---
layout: ../../layouts/Doc.astro
title: Agent recipes — MCP cookbook
subtitle: Concrete, copy-pastable snippets for the most common patterns. Each recipe is ~10–30 lines of Python over the local JSON-RPC socket. No SDK, no install, just sockets.
kicker: Docs / Agent recipes
date: 2026-05-19
---

## What this page is

Unterm's MCP server is a plain TCP socket on `127.0.0.1` speaking newline-delimited JSON-RPC. Anyone — Claude Code, Cursor, a Python script in cron, a shell function piped through `nc` — can drive every part of the terminal from outside. The methods are documented exhaustively in the [MCP reference](/docs/mcp-reference); this page is the opposite: a small handful of complete recipes you actually run.

Every snippet here assumes you've read the auth token from `~/.unterm/instances/<name>.json` (or `~/.unterm/server.json` if you only care about the currently-active window). The recipes use Python's stdlib `socket` module so they're self-contained — no `pip install` needed.

A tiny shared helper:

```python
import socket, json

def mcp_open(host="127.0.0.1", port=19876, token=""):
    s = socket.create_connection((host, port))
    call(s, "auth.login", {"token": token})
    return s

def call(s, method, params=None, _id=[1]):
    req = {"jsonrpc": "2.0", "id": _id[0], "method": method, "params": params or {}}
    _id[0] += 1
    s.sendall((json.dumps(req) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    return json.loads(buf.decode().split("\n", 1)[0])
```

All recipes below assume `s` is an already-authenticated socket.

---

## 1. Split right, focus, drive

The reflexive demo from the landing page, distilled. Splits the calling pane to the right, makes the new pane active, types a command into it, reads back the result.

```python
import time

def split_drive_read(s, src_pane_id, command, direction="right"):
    # 1. Split — direction is right | left | down | up
    new = call(s, "session.split", {
        "id": src_pane_id,
        "direction": direction,
        "cwd": "/path/to/repo",
    })["result"]
    pid = new["id"]

    # 2. Focus so the user can see the hand-off
    call(s, "session.focus", {"id": pid})

    # 3. Type — character-by-character produces visible "human" rhythm.
    #    If you want it fast, send the whole string in one call.
    for ch in command + "\n":
        call(s, "session.input", {"id": pid, "input": ch})
        time.sleep(0.03)

    # 4. Give the shell a beat to execute, then read screen text.
    time.sleep(0.6)
    return call(s, "screen.text", {"id": pid})["result"]["lines"]
```

**Use it for:** showing a side-by-side pane to a viewer (live demo), running a build in a separate pane while keeping the original pane free for chat, opening a target pane for `git diff` inspection.

---

## 2. Director-and-worker pattern

Director sits in pane `#0`, spawns one or more worker panes, watches each idle state, dispatches the next task when a worker frees up.

```python
def spawn_worker(s, src=0, label="worker"):
    new = call(s, "session.split", {"id": src, "direction": "right"})["result"]
    pid = new["id"]
    # Tag the pane title so we can find it later by name not id
    call(s, "session.input", {"id": pid, "input": f"echo \"{label} ready\"\n"})
    return pid

def wait_idle(s, pid, timeout_s=120):
    """Block until the shell prompt is back. session.idle returns
    {idle: bool, last_activity_ms_ago: int}."""
    import time as _t
    start = _t.time()
    while _t.time() - start < timeout_s:
        r = call(s, "session.idle", {"id": pid})["result"]
        if r["idle"]:
            return True
        _t.sleep(0.5)
    return False

def dispatch(s, pid, command):
    for ch in command + "\n":
        call(s, "session.input", {"id": pid, "input": ch})

# Pattern:
workers = [spawn_worker(s, label=f"w{i}") for i in range(3)]
tasks = ["cargo test -p unterm-profile",
         "pnpm build",
         "ci/selftest-profiles.sh"]
for w, task in zip(workers, tasks):
    dispatch(s, w, task)
for w in workers:
    wait_idle(s, w)
    print(f"pane {w} done. tail:", call(s, "screen.text", {"id": w})["result"]["lines"][-3:])
```

**Why this is interesting:** the director itself can be an LLM that decides what task to give each worker based on the previous worker's output. The whole loop is one Python script.

---

## 3. Read a pane's recent output as Markdown

Useful for grabbing a transcript to feed back into an LLM, or to paste into a bug report.

```python
md = call(s, "session.export_markdown", {"id": 5, "max_blocks": 10})["result"]
print(md["markdown"])
```

`session.export_markdown` honors OSC 133 block boundaries — you get nicely separated "command + output" pairs instead of a raw scrollback dump. Token-redacted by default (the same redaction pass session recordings use).

---

## 4. Capture a screenshot of one pane

Region-shot the contents of any pane to disk. The image is what's currently rendered, including cursor and selection state.

```python
r = call(s, "capture.window", {"id": 5})["result"]
print("PNG path:", r["path"])
# r["path"] is in ~/.unterm/screenshots/ and is also on the image clipboard
```

For the whole Unterm window: `capture.screen`. For a user-drawn rectangle (interactive): `capture.select` — blocks until the user finishes dragging.

---

## 5. Wait for a specific string to appear

Cheap polling instead of an event subscription — works for "wait for the test runner to print PASS" type checks.

```python
import time

def wait_for_text(s, pid, needle, timeout_s=60):
    start = time.time()
    while time.time() - start < timeout_s:
        text = "\n".join(call(s, "screen.text", {"id": pid})["result"]["lines"])
        if needle in text:
            return True
        time.sleep(0.4)
    return False

wait_for_text(s, 5, "test result: ok.", timeout_s=120)
```

For a richer search API see `screen.search` in the [MCP reference](/docs/mcp-reference) — it returns stable row offsets and can jump the user-visible viewport to a match (`goto` / `goto_match`). It is substring-only, not regex.

---

## 6. Propose text without writing it (v0.17+)

`session.suggest` is for when you want to offer a command to the user without injecting it. The user accepts with `Tab` (or rejects with `Esc`). The shell never sees the text unless the user explicitly accepts.

```python
r = call(s, "session.suggest", {
    "id": 5,
    "text": "git push origin master",
    "ttl_ms": 30000,  # disappears after 30s if not acted on
})["result"]
sid = r["suggestion_id"]

# Poll for resolution:
while True:
    st = call(s, "session.suggest_status", {"id": sid})["result"]
    if st["state"] != "Pending":
        print("resolved:", st["state"])  # "Accepted" / "Dismissed" / "Expired"
        break
    import time as _t; _t.sleep(0.5)
```

**Use this instead of `session.input`** for any command that has side effects you don't want to ship without consent — `git push`, `rm -rf`, `aws s3 rb`, anything destructive. The first time a new agent uses `session.input` Unterm pops a confirmation banner anyway; suggest skips that friction by being non-destructive by definition.

---

## 7. Tag your agent

Without identifying yourself, your activity in the audit log groups under `anonymous`. With:

```python
call(s, "agent.identify", {"name": "claude-code", "version": "0.42"})
```

Audit entries now show `agent=claude-code`. The `mcp:N` chip in the status bar attributes writes to you specifically. Users can decide to "Always allow" you via `Alt+A` on the confirmation banner.

**Naming convention:** lowercase, hyphenated, no spaces. `claude-code` not `Claude Code 0.42`. Append a version separately so the user can recognize the agent even when you upgrade.

---

## 8. Enumerate windows and panes across all Unterm instances

When the user has multiple Unterm windows open (alpha, bravo, charlie, …), each runs its own MCP server on a different port. Cross-instance discovery is via the `~/.unterm/instances/` directory:

```python
import os, json, glob

def all_instances():
    for path in glob.glob(os.path.expanduser("~/.unterm/instances/*.json")):
        with open(path) as f:
            yield json.load(f)

for inst in all_instances():
    print(f"  {inst['id']:8}  pid={inst['pid']:6}  http=:{inst['http_port']}  cwd={inst.get('cwd')}")
```

Within one instance, `instance.list` is the equivalent MCP method (returns the same JSON). `instance.info` returns just the current one.

---

## 9. Drive a profile-bound shell

When a window is bound to an identity profile (v0.13+), every pane in it inherits the profile's env. Agents can spawn shells in a specific identity:

```bash
unterm-cli profile spawn "Work — Acme" --cwd /Volumes/Dev/code/work
# this opens a NEW window bound to the named profile;
# from there the agent connects to that window's MCP port
```

To find the right port + token for the newly-opened window, watch `~/.unterm/instances/` for a new JSON file appearing.

---

## See also

- [MCP reference](/docs/mcp-reference) — every method, every parameter
- [Multi-instance](/docs/multi-instance) — discovery protocol, NATO names
- [Profiles](/docs/profiles) — identity binding, keychain, ghost text
- [Architecture](/docs/architecture) — how the pieces fit
- [Config file reference](/docs/config-reference) — every key in `unterm.conf`
