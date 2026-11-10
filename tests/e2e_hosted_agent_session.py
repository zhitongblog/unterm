"""End-to-end: an MCP client drives a hosted agent session through
unterm's real product path (unterm-core --headless + unterm-cli mcp-stdio).

Agents: `claude` (default; needs a logged-in claude, spends a few cents of
haiku), `codex` (`codex app-server`, signed in) `agy` (ACP over `agy acp`, signed in) or `opencode` (ACP over `opencode acp`, on OpenCode's free model).
Run against an isolated Core:

    export UNTERM_STATE_DIR=$(mktemp -d)
    target/debug/unterm-core --headless &
    python -I tests/e2e_hosted_agent_session.py target/debug/unterm-cli <workdir> <logfile> [claude|codex|opencode|agy]

usage: python -I e2e_hosted_agent_session.py <unterm-cli> <workdir> <logfile> [agent]

Scenario A (allow): ask Claude to write out.txt -> approval.requested ->
  decide(allow) -> file exists -> submit_input follow-up -> close.
Scenario B (deny+interrupt): same ask -> decide(deny, interrupt) -> no file,
  turn interrupted, no second question -> close.
Scenario C (unanswered): ask -> close without answering -> auto-deny, close
  returns (does not hang).
"""
import json, os, subprocess, sys, time

cli, root, logfile = sys.argv[1:4]
agent = sys.argv[4] if len(sys.argv) > 4 else "claude"
if agent == "claude":
    COMMAND, ENV = ["claude", "-p", "--model", "haiku"], {}
elif agent == "opencode":
    # Through cmd /c on Windows: opencode is an npm .cmd shim. unterm turns
    # `opencode` into `opencode acp` and sets its tools to ask.
    COMMAND = (["cmd", "/c", "opencode"] if os.name == "nt" else ["opencode"])
    ENV = {"OPENCODE_CONFIG_CONTENT": json.dumps({"model": "opencode/deepseek-v4.1-flash-free"})}
elif agent == "codex":
    # unterm turns `codex` into `codex app-server` (approval policy untrusted).
    COMMAND, ENV = (["cmd", "/c", "codex"] if os.name == "nt" else ["codex"]), {}
elif agent == "agy":
    # unterm turns `agy` into `agy acp`; agy asks before commands by default.
    COMMAND, ENV = ["agy"], {}
else:
    sys.exit(f"unknown agent {agent!r}: claude, codex, opencode or agy")
log = open(logfile, "w", encoding="utf-8")

def note(*parts):
    line = " ".join(str(p) for p in parts)
    print(line, flush=True)
    log.write(line + "\n"); log.flush()

bridge = subprocess.Popen([cli, "mcp-stdio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)
next_id = 0

def rpc(method, params=None):
    global next_id
    next_id += 1
    msg = {"jsonrpc": "2.0", "id": next_id, "method": method}
    if params is not None:
        msg["params"] = params
    bridge.stdin.write(json.dumps(msg) + "\n"); bridge.stdin.flush()
    while True:
        line = bridge.stdout.readline()
        if not line:
            raise RuntimeError("bridge closed: " + bridge.stderr.read()[:2000])
        reply = json.loads(line)
        if reply.get("id") == next_id:
            if "error" in reply:
                raise RuntimeError(f"{method}: {reply['error']}")
            return reply["result"]

def call(tool, args):
    result = rpc("tools/call", {"name": tool, "arguments": args})
    text = "".join(c.get("text", "") for c in result.get("content", []))
    if result.get("isError"):
        raise RuntimeError(f"{tool} -> {text[:800]}")
    try:
        return json.loads(text)
    except Exception:
        return text

def events_until(sid, pred, timeout=120):
    deadline = time.time() + timeout
    while True:
        page = call(TOOLS["events"], {"session_id": sid, "cursor": 0})
        evs = page.get("events", page) if isinstance(page, dict) else page
        if pred(evs) or time.time() > deadline:
            return evs
        time.sleep(0.3)

def names(evs):
    return [e.get("event") for e in evs]

# --- handshake
init = rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                          "clientInfo": {"name": "unzoo-one-e2e", "version": "0"}})
note("initialize:", init.get("serverInfo"))
bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"); bridge.stdin.flush()
tools = [t["name"] for t in rpc("tools/list")["tools"]]
note("tools/list:", len(tools), "tools;", [t for t in tools if "agent_session" in t])
TOOLS = {}
for short in ["start", "events", "decide", "submit_input", "status", "close"]:
    match = [t for t in tools if t.endswith("agent_session." + short) or t.endswith("agent_session_" + short)]
    assert match, f"no agent_session.{short} tool"
    TOOLS[short] = match[0]

TASK_IDS = {'allow': 'task_01JBQ8XG4T7N2M9V0KRSD3WZ1A', 'deny': 'task_01JBQ8XG4T7N2M9V0KRSD3WZ1B', 'unanswered': 'task_01JBQ8XG4T7N2M9V0KRSD3WZ1C'}  # contract-shaped ids
ASK = "Use the bash tool to run exactly this command: echo hi > out.txt . Then reply with only: written"
results = {}

def start_session(name):
    work = os.path.join(root, name)
    os.makedirs(work, exist_ok=True)
    res = call(TOOLS["start"], {"command": COMMAND, "env": ENV, "hosted": True,
                                "cwd": work, "prompt": ASK, "task_id": TASK_IDS[name]})
    sid = res["session_id"]
    note(f"[{name}] started", sid)
    evs = events_until(sid, lambda e: "approval.requested" in names(e))
    asked = next(e for e in evs if e.get("event") == "approval.requested")
    note(f"[{name}] approval.requested:", json.dumps(asked["approval"]["tool_call"]), "turn", asked.get("turn_id"))
    st = call(TOOLS["status"], {"session_id": sid})
    note(f"[{name}] status: hosted={st.get('hosted')} pending={st.get('pending_approvals')}")
    return sid, work, asked

# --- A: allow, then a follow-up turn
sid, work, asked = start_session("allow")
t0 = time.time()
res = call(TOOLS["decide"], {"session_id": sid, "request_id": asked["approval"]["request_id"], "allow": True})
note("[allow] decide ->", json.dumps(res.get("approval")))
evs = events_until(sid, lambda e: names(e).count("turn.completed") >= 1)
note(f"[allow] turn 1 done in {time.time()-t0:.1f}s, out.txt exists:", os.path.exists(os.path.join(work, "out.txt")))
call(TOOLS["submit_input"], {"session_id": sid, "text": "Now reply with only the word: done"})
evs = events_until(sid, lambda e: names(e).count("turn.completed") >= 2)
reply = "".join(e.get("text", "") for e in evs if e.get("event") == "output.delta" and e.get("stream") == "assistant")
note("[allow] assistant said:", repr(reply))
end = call(TOOLS["close"], {"session_id": sid})
note("[allow] close ->", end.get("state"))
results["allow"] = os.path.exists(os.path.join(work, "out.txt")) and reply.strip().endswith("done") and end.get("state") == "finished"
json.dump(evs, open(logfile + ".allow.json", "w", encoding="utf-8"), indent=1)

# --- B: deny and stop the turn
sid, work, asked = start_session("deny")
t0 = time.time()
call(TOOLS["decide"], {"session_id": sid, "request_id": asked["approval"]["request_id"], "allow": False,
                       "message": "not on this machine", "interrupt": True})
evs = events_until(sid, lambda e: "turn.completed" in names(e))
done = next(e for e in evs if e.get("event") == "turn.completed")
asks = names(evs).count("approval.requested")
note(f"[deny] turn ended in {time.time()-t0:.2f}s state={done.get('state')} questions={asks} out.txt exists:",
     os.path.exists(os.path.join(work, "out.txt")))
end = call(TOOLS["close"], {"session_id": sid})
results["deny"] = (not os.path.exists(os.path.join(work, "out.txt"))) and done.get("state") == "interrupted" and asks == 1
json.dump(evs, open(logfile + ".deny.json", "w", encoding="utf-8"), indent=1)

# --- C: nobody answers; close must not hang
sid, work, asked = start_session("unanswered")
t0 = time.time()
end = call(TOOLS["close"], {"session_id": sid})
note(f"[unanswered] close returned in {time.time()-t0:.2f}s state={end.get('state')} out.txt exists:",
     os.path.exists(os.path.join(work, "out.txt")))
note(f"[unanswered] ending: exit_code={end.get('exit_code')} reason={end.get('reason')!r}")
results["unanswered"] = (time.time() - t0 < 60 and not os.path.exists(os.path.join(work, "out.txt"))
                         and end.get("state") == "interrupted" and bool(end.get("reason")))

# --- a stale/duplicate answer is refused
try:
    call(TOOLS["decide"], {"session_id": sid, "request_id": asked["approval"]["request_id"], "allow": True})
    results["stale_refused"] = False
except RuntimeError as e:
    note("[unanswered] answering after close refused:", str(e)[:160])
    results["stale_refused"] = True

note("RESULTS:", json.dumps(results))
bridge.stdin.close()
bridge.wait(timeout=10)
sys.exit(0 if all(results.values()) else 1)
