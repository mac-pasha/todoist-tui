#!/usr/bin/env python3
"""Task graph tool. No deps beyond stdlib.

  graph.py validate <file>              schema-lite checks, dup ids, missing deps, cycles
  graph.py next [EPIC-xx|file]          tasks with status todo and all deps done
  graph.py show T-xxx                   print task JSON (searches docs/tasks)
  graph.py set T-xxx <status> [--reason "..."]
  graph.py status                       summary across all graphs
"""
import json, sys, glob, os

TASK_DIR = "docs/tasks"
STATUSES = {"todo", "in_progress", "review", "done", "blocked"}
TYPES = {"code", "design", "test-int", "test-e2e", "docs"}
REQ = ["id", "title", "type", "deps", "files", "acceptance", "status"]
AGENT_MAX = {"todo", "in_progress", "review", "blocked"}  # 'done' only by human (--human)


def die(msg, code=1):
    print(f"✗ {msg}", file=sys.stderr); sys.exit(code)


def load(path):
    with open(path) as f:
        return json.load(f)


def save(path, g):
    with open(path, "w") as f:
        json.dump(g, f, indent=2, ensure_ascii=False); f.write("\n")


def all_graphs():
    return sorted(glob.glob(os.path.join(TASK_DIR, "*.graph.json")))


def resolve(arg):
    if arg is None:
        return all_graphs()
    if os.path.isfile(arg):
        return [arg]
    p = os.path.join(TASK_DIR, f"{arg}.graph.json")
    if os.path.isfile(p):
        return [p]
    die(f"no graph for {arg}")


def global_status():
    """task id -> status across all graphs (cross-epic deps allowed)."""
    m = {}
    for p in all_graphs():
        for t in load(p).get("tasks", []):
            m[t["id"]] = t["status"]
    return m


def validate(path):
    g = load(path); errs = []
    for k in ("epic", "title", "tasks"):
        if k not in g: errs.append(f"missing top-level '{k}'")
    tasks = g.get("tasks", [])
    ids = [t.get("id") for t in tasks]
    dups = {i for i in ids if ids.count(i) > 1}
    if dups: errs.append(f"duplicate ids: {sorted(dups)}")
    known = set(ids) | set(global_status())
    for t in tasks:
        tid = t.get("id", "?")
        for k in REQ:
            if k not in t: errs.append(f"{tid}: missing '{k}'")
        if t.get("type") not in TYPES: errs.append(f"{tid}: bad type {t.get('type')}")
        if t.get("status") not in STATUSES: errs.append(f"{tid}: bad status {t.get('status')}")
        if not t.get("files"): errs.append(f"{tid}: empty files")
        if not t.get("acceptance"): errs.append(f"{tid}: empty acceptance")
        for d in t.get("deps", []):
            if d not in known: errs.append(f"{tid}: unknown dep {d}")
            if d == tid: errs.append(f"{tid}: depends on itself")
    # cycle check (DFS) within this graph
    adj = {t["id"]: [d for d in t.get("deps", []) if d in ids] for t in tasks if "id" in t}
    state = {}
    def dfs(n, stack):
        state[n] = 1
        for d in adj.get(n, []):
            if state.get(d) == 1: errs.append(f"cycle: {' -> '.join(stack + [n, d])}")
            elif d not in state: dfs(d, stack + [n])
        state[n] = 2
    for n in adj:
        if n not in state: dfs(n, [])
    if errs:
        for e in errs: print(f"✗ {e}")
        sys.exit(1)
    print(f"✓ {path}: {len(tasks)} tasks valid")


def next_tasks(arg):
    st = global_status(); out = []
    for p in resolve(arg):
        g = load(p)
        for t in g["tasks"]:
            if t["status"] == "todo" and all(st.get(d) == "done" for d in t["deps"]):
                out.append((g["epic"], t))
    if not out:
        print("no ready tasks"); return
    for epic, t in out:
        print(f"{t['id']}\t{t['type']}\t{epic}\t{t['title']}")


def find(tid):
    for p in all_graphs():
        g = load(p)
        for t in g["tasks"]:
            if t["id"] == tid:
                return p, g, t
    die(f"task {tid} not found")


def show(tid):
    p, g, t = find(tid)
    print(json.dumps({"epic": g["epic"], "graph": p, **t}, indent=2, ensure_ascii=False))


def set_status(tid, status, rest):
    human = "--human" in rest
    if status not in STATUSES: die(f"bad status {status}")
    if status == "done" and not human:
        die("only humans set 'done' (add --human after merge)")
    p, g, t = find(tid)
    if status == "blocked":
        if "--reason" not in rest: die("blocked needs --reason \"...\"")
        t["blocked_reason"] = rest[rest.index("--reason") + 1]
    else:
        t.pop("blocked_reason", None)
    old = t["status"]; t["status"] = status; save(p, g)
    print(f"✓ {tid}: {old} → {status}")


def status():
    graphs = all_graphs()
    if not graphs:
        print("no graphs in docs/tasks"); return
    for p in graphs:
        g = load(p); c = {s: 0 for s in ["done", "review", "in_progress", "todo", "blocked"]}
        for t in g["tasks"]: c[t["status"]] += 1
        print(f"{g['epic']} {g['title']}: " + " ".join(f"{k}={v}" for k, v in c.items()))
        for t in g["tasks"]:
            if t["status"] == "blocked":
                print(f"   ⛔ {t['id']} {t['title']} — {t.get('blocked_reason','')}")


if __name__ == "__main__":
    a = sys.argv[1:]
    if not a: print(__doc__); sys.exit(0)
    cmd = a[0]
    if cmd == "validate" and len(a) == 2: validate(a[1])
    elif cmd == "next": next_tasks(a[1] if len(a) > 1 else None)
    elif cmd == "show" and len(a) == 2: show(a[1])
    elif cmd == "set" and len(a) >= 3: set_status(a[1], a[2], a[3:])
    elif cmd == "status": status()
    else: print(__doc__); sys.exit(2)
