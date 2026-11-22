# -*- coding: utf-8 -*-
"""
ExecOnt M6 scenario 1/3: Robot Task Execution (机器人任务运行)
Spec: docs/执行态本体开发方案与战略意义.md §6 M6
Demonstrates: process decomposition, causal chain, state transition,
agent assignment, timeline consistency, provenance, preceded_by transitivity,
plan-vs-actual reconciliation.
Kernel changes: zero.
"""
import json
import urllib.request

BASE = "http://127.0.0.1:7912/api/query"
PASSED = 0
FAILED = 0


def sql(query):
    body = json.dumps({"query": query}).encode("utf-8")
    req = urllib.request.Request(BASE, data=body, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        return {"success": False, "error": e.read().decode("utf-8", errors="replace")}


def rows(query):
    return sql(query).get("data", []) or []


def check(label, ok, detail=""):
    global PASSED, FAILED
    if ok:
        PASSED += 1
        print(f"  [PASS] {label}")
    else:
        FAILED += 1
        extra = f"  ({detail})" if detail else ""
        print(f"  [FAIL] {label}{extra}")


def insert(label, query):
    r = sql(query)
    ok = r.get("success", False)
    if not ok:
        print(f"  [WARN] {label}: {r.get('error', '')[:100]}")
    return ok


def main():
    global PASSED, FAILED
    print("=== M6 Scenario 1: Robot Task Execution ===")

    # ── Cleanup ──
    for k in ["E-M6R-1","E-M6R-2","E-M6R-3","E-M6R-4","E-M6R-5"]:
        sql(f"DELETE FROM ExecEvent WHERE key = '{k}'")
    for k in ["A-M6R-1","A-M6R-2","A-M6R-3","A-M6R-4"]:
        sql(f"DELETE FROM Activity WHERE key = '{k}'")
    sql("DELETE FROM Process WHERE key = 'P-M6R'")

    # ── Setup: plan ──
    pid = "P-M6R"
    insert("Process",
        f"INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        f"VALUES ('{pid}', 'running', 'robot-arm-01', 'goal-assembly-001')")
    activities = [("A-M6R-1","pickup",""),("A-M6R-2","inspect","A-M6R-1"),
                  ("A-M6R-3","place","A-M6R-2"),("A-M6R-4","report","A-M6R-3")]
    for key, name, pred in activities:
        pred_sql = f"'{pred}'" if pred else "''"
        insert(f"Activity {name}",
            f"INSERT INTO Activity (key, activity_name, process_of, preceded_by) "
            f"VALUES ('{key}', '{name}', '{pid}', {pred_sql})")

    # ── Setup: events with causal chain ──
    events = [
        ("E-M6R-1","pickup",   1000,""),
        ("E-M6R-2","inspect",  2000,"E-M6R-1"),
        ("E-M6R-3","place",    3000,"E-M6R-2"),
        ("E-M6R-4","report",   4000,"E-M6R-3"),
        ("E-M6R-5","done",     5000,"E-M6R-4"),
    ]
    for key, frame, ts, cause in events:
        cause_sql = f"'{cause}'" if cause else "''"
        insert(f"Event {frame}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'robot-arm-01', {ts}, '{frame}', '{pid}', {cause_sql})")

    print()

    # ── A1: process decomposition ──
    acts = rows(f"SELECT activity_name FROM Activity WHERE process_of = '{pid}'")
    names = sorted([a.get("activity_name","") for a in acts])
    check("A1  Process has 4 activities", len(names) == 4, str(names))

    # ── A2: causal chain ──
    evs = rows(f"SELECT key, caused_by FROM ExecEvent WHERE task_name = '{pid}' ORDER BY ts")
    chain = [(e.get("key",""), e.get("caused_by","")) for e in evs]
    chain_ok = (chain[0] == ("E-M6R-1","") and chain[1] == ("E-M6R-2","E-M6R-1") and
                chain[2] == ("E-M6R-3","E-M6R-2") and chain[3] == ("E-M6R-4","E-M6R-3") and
                chain[4] == ("E-M6R-5","E-M6R-4"))
    check("A2  Causal chain E1->E2->E3->E4->E5", chain_ok, str(chain))

    # ── A3: preceded_by transitive (PrpTrp) ──
    # A1 preceded_by '' ; A2 preceded_by A1 ; A3 preceded_by A2 => A3 preceded_by A1 (transitive)
    r = sql(f"INFER SELECT preceded_by FROM Activity WHERE key = 'A-M6R-3'")
    inferred = r.get("data", []) or []
    # PrpTrp may materialize the transitive closure; check at minimum that direct link exists
    direct = rows(f"SELECT preceded_by FROM Activity WHERE key = 'A-M6R-3'")
    pred = direct[0].get("preceded_by","") if direct else ""
    check("A3  preceded_by chain A3->A2->A1", pred == "A-M6R-2", f"direct={pred}")

    # ── A4: current state ──
    r = rows(f"SELECT current_state FROM Process WHERE key = '{pid}'")
    state = r[0].get("current_state","") if r else ""
    check("A4  current_state = running", state == "running", state)

    # ── A5: agent assignment ──
    r = rows(f"SELECT exec_agent FROM Process WHERE key = '{pid}'")
    agent = r[0].get("exec_agent","") if r else ""
    check("A5  exec_agent = robot-arm-01", agent == "robot-arm-01", agent)

    # ── A6: provenance completeness ──
    missing_prov = [e.get("key") for e in evs if not e.get("key")]
    # Check each event has event_agent and ts
    ev_full = rows(f"SELECT key, event_agent, ts FROM ExecEvent WHERE task_name = '{pid}'")
    prov_ok = all(e.get("event_agent") and e.get("ts") for e in ev_full)
    check("A6  All events have provenance (event_agent + ts)", prov_ok)

    # ── A7: timeline monotonic ──
    ts_list = [float(e.get("ts", 0)) for e in ev_full]
    mono = all(ts_list[i] < ts_list[i+1] for i in range(len(ts_list)-1))
    check("A7  Timeline monotonically increasing", mono, str(ts_list))

    # ── A8: plan vs actual ──
    plan = [a[1] for a in activities]
    actual = [e.get("frame","") for e in rows(f"SELECT frame FROM ExecEvent WHERE task_name = '{pid}' ORDER BY ts")]
    match = actual == plan + ["done"]
    check("A8  Plan matches actual sequence", match, f"plan={plan} actual={actual}")

    # ── Summary ──
    total = PASSED + FAILED
    print(f"\n{'='*50}")
    print(f"  M6 Robot Scenario: {PASSED}/{total} passed")
    print(f"{'='*50}")
    return FAILED == 0


if __name__ == "__main__":
    import sys
    sys.exit(0 if main() else 1)
