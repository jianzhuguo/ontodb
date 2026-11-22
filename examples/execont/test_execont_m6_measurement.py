# -*- coding: utf-8 -*-
"""
ExecOnt M6 scenario 3/3: Measurement Process Execution (测量工序执行)
Spec: docs/执行态本体开发方案与战略意义.md §6 M6
Demonstrates: measurement sequence compliance, tolerance guard check,
plan-vs-actual reconciliation, missing-step detection, out-of-bounds detection,
evidence chain, causal chain.
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
    print("=== M6 Scenario 3: Measurement Process Execution ===")

    # ── Plan definition ──
    PLAN = ["setup", "measure", "analyze", "report"]
    TOLERANCE = 0.05

    # ── Cleanup ──
    for prefix in ["E-M6M1-", "E-M6M2-", "E-M6M3-"]:
        for i in range(1, 6):
            sql(f"DELETE FROM ExecEvent WHERE key = '{prefix}{i}'")
    for k in ["A-M6M-1","A-M6M-2","A-M6M-3","A-M6M-4"]:
        sql(f"DELETE FROM Activity WHERE key = '{k}'")
    for k in ["T-M6M-guard"]:
        sql(f"DELETE FROM Transition WHERE key = '{k}'")
    for pid in ["P-M6M1","P-M6M2","P-M6M3"]:
        sql(f"DELETE FROM Process WHERE key = '{pid}'")

    # ── Setup: shared plan activities ──
    for i, step in enumerate(PLAN, 1):
        pred = f"'A-M6M-{i-1}'" if i > 1 else "''"
        insert(f"Activity {step}",
            f"INSERT INTO Activity (key, activity_name, process_of, preceded_by) "
            f"VALUES ('A-M6M-{i}', '{step}', 'P-M6M1', {pred})")

    # ── Setup: guard transition ──
    insert("Transition with tolerance guard",
        "INSERT INTO Transition (key, from_state, to_state, guard) "
        "VALUES ('T-M6M-guard', 'measure', 'analyze', 'tolerance <= 0.05')")

    # ── Scenario A: compliant (all within tolerance) ──
    insert("Process P-M6M1 (compliant)",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-M6M1', 'done', 'cmm-01', 'goal-inspect-001')")
    events_a = [
        ("E-M6M1-1", "setup",   1000, "",            ""),
        ("E-M6M1-2", "measure", 2000, "E-M6M1-1",   "0.02"),
        ("E-M6M1-3", "analyze", 3000, "E-M6M1-2",   ""),
        ("E-M6M1-4", "report",  4000, "E-M6M1-3",   ""),
    ]
    for key, frame, ts, cause, val in events_a:
        cause_sql = f"'{cause}'" if cause else "''"
        # Encode measurement value in frame for tolerance check
        frame_full = f"{frame}:{val}" if val else frame
        insert(f"Event {frame}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'cmm-01', {ts}, '{frame_full}', 'P-M6M1', {cause_sql})")

    # ── Scenario B: violation (measure out of tolerance) ──
    insert("Process P-M6M2 (violation)",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-M6M2', 'done', 'cmm-01', 'goal-inspect-002')")
    events_b = [
        ("E-M6M2-1", "setup",   1000, "",            ""),
        ("E-M6M2-2", "measure", 2000, "E-M6M2-1",   "0.12"),
        ("E-M6M2-3", "analyze", 3000, "E-M6M2-2",   ""),
        ("E-M6M2-4", "report",  4000, "E-M6M2-3",   ""),
    ]
    for key, frame, ts, cause, val in events_b:
        cause_sql = f"'{cause}'" if cause else "''"
        frame_full = f"{frame}:{val}" if val else frame
        insert(f"Event {frame}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'cmm-01', {ts}, '{frame_full}', 'P-M6M2', {cause_sql})")

    # ── Scenario C: missing step (no analyze) ──
    insert("Process P-M6M3 (missing step)",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-M6M3', 'done', 'cmm-01', 'goal-inspect-003')")
    events_c = [
        ("E-M6M3-1", "setup",   1000, "",            ""),
        ("E-M6M3-2", "measure", 2000, "E-M6M3-1",   "0.03"),
        ("E-M6M3-3", "report",  4000, "E-M6M3-2",   ""),  # analyze missing
    ]
    for key, frame, ts, cause, val in events_c:
        cause_sql = f"'{cause}'" if cause else "''"
        frame_full = f"{frame}:{val}" if val else frame
        insert(f"Event {frame}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'cmm-01', {ts}, '{frame_full}', 'P-M6M3', {cause_sql})")

    print()

    # ── A1: plan sequence correct ──
    acts = rows("SELECT activity_name FROM Activity WHERE process_of = 'P-M6M1' ORDER BY key")
    plan_actual = [a.get("activity_name","") for a in acts]
    check("A1  Plan sequence = setup->measure->analyze->report",
          plan_actual == PLAN, str(plan_actual))

    # ── A2: Scenario A compliant (all within tolerance) ──
    evts_a = rows("SELECT frame FROM ExecEvent WHERE task_name = 'P-M6M1' ORDER BY ts")
    frames_a = [e.get("frame","") for e in evts_a]
    # Parse measurement value
    meas_val_a = None
    for f in frames_a:
        if f.startswith("measure:"):
            meas_val_a = float(f.split(":",1)[1])
    compliant_a = meas_val_a is not None and meas_val_a <= TOLERANCE
    check("A2  Scenario A: measurement within tolerance",
          compliant_a, f"value={meas_val_a} tolerance={TOLERANCE}")

    # ── A3: Scenario B violation (out of tolerance) ──
    evts_b = rows("SELECT frame FROM ExecEvent WHERE task_name = 'P-M6M2' ORDER BY ts")
    frames_b = [e.get("frame","") for e in evts_b]
    meas_val_b = None
    for f in frames_b:
        if f.startswith("measure:"):
            meas_val_b = float(f.split(":",1)[1])
    violation_b = meas_val_b is not None and meas_val_b > TOLERANCE
    check("A3  Scenario B: out-of-bounds detected",
          violation_b, f"value={meas_val_b} tolerance={TOLERANCE}")

    # ── A4: Scenario C missing step ──
    evts_c = rows("SELECT frame FROM ExecEvent WHERE task_name = 'P-M6M3' ORDER BY ts")
    frames_c = [e.get("frame","").split(":")[0] for e in evts_c]  # strip value suffix
    missing = [s for s in PLAN if s not in frames_c]
    check("A4  Scenario C: missing step 'analyze' detected",
          missing == ["analyze"], f"missing={missing}")

    # ── A5: plan vs actual (scenario A) ──
    actual_steps_a = [f.split(":")[0] for f in frames_a]
    match_a = actual_steps_a == PLAN
    check("A5  Scenario A: plan matches actual", match_a,
          f"plan={PLAN} actual={actual_steps_a}")

    # ── A6: causal chain (scenario A) ──
    chain_a = rows("SELECT key, caused_by FROM ExecEvent WHERE task_name = 'P-M6M1' ORDER BY ts")
    chain_ok = (chain_a[0].get("caused_by","") == "" and
                chain_a[1].get("caused_by","") == "E-M6M1-1" and
                chain_a[2].get("caused_by","") == "E-M6M1-2" and
                chain_a[3].get("caused_by","") == "E-M6M1-3")
    check("A6  Scenario A: causal chain E1->E2->E3->E4", chain_ok)

    # ── A7: provenance completeness (all scenarios) ──
    all_evts = rows("SELECT event_agent, ts FROM ExecEvent WHERE task_name IN ('P-M6M1','P-M6M2','P-M6M3')")
    prov_ok = all(e.get("event_agent") and e.get("ts") for e in all_evts)
    check("A7  All events have provenance (event_agent + ts)", prov_ok)

    # ── Summary ──
    total = PASSED + FAILED
    print(f"\n{'='*50}")
    print(f"  M6 Measurement Scenario: {PASSED}/{total} passed")
    print(f"{'='*50}")
    return FAILED == 0


if __name__ == "__main__":
    import sys
    sys.exit(0 if main() else 1)
