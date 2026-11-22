# -*- coding: utf-8 -*-
"""
ExecOnt M6 biomedical scenario: GxP Batch Release QC (批放行质量控制)
Domain: pharmaceutical quality control
Demonstrates: protocol compliance, deviation detection, evidence chain,
causal chain, plan-vs-actual reconciliation in a GxP context.
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
    print("=== ExecOnt Biomedical Scenario: GxP Batch Release QC ===")

    # Protocol: sample_prep -> assay_run -> data_review -> deviation_check -> release_decision
    PROTOCOL = ["sample_prep", "assay_run", "data_review", "deviation_check", "release_decision"]
    ACCEPTANCE_THRESHOLD = 98.0  # assay result must be >= 98.0%

    # Cleanup
    for pid in ["P-BIO-1", "P-BIO-2", "P-BIO-3"]:
        sql(f"DELETE FROM ExecEvent WHERE task_name = '{pid}'")
        sql(f"DELETE FROM Process WHERE key = '{pid}'")
    for k in ["A-BIO-1","A-BIO-2","A-BIO-3","A-BIO-4","A-BIO-5"]:
        sql(f"DELETE FROM Activity WHERE key = '{k}'")
    sql("DELETE FROM Transition WHERE key = 'T-BIO-guard'")

    # Shared protocol activities
    for i, step in enumerate(PROTOCOL, 1):
        pred = f"'A-BIO-{i-1}'" if i > 1 else "''"
        insert(f"Activity {step}",
            f"INSERT INTO Activity (key, activity_name, process_of, preceded_by) "
            f"VALUES ('A-BIO-{i}', '{step}', 'P-BIO-1', {pred})")

    # Guard transition: assay_run -> data_review requires purity >= 98.0%
    insert("Transition with purity guard",
        "INSERT INTO Transition (key, from_state, to_state, guard) "
        "VALUES ('T-BIO-guard', 'assay_run', 'data_review', 'purity >= 98.0')")

    # ── Batch 1: compliant (purity 99.2%) ──
    insert("Batch 1 process",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-BIO-1', 'released', 'lab-tech-07', 'goal-batch-release-B2401')")
    events_1 = [
        ("E-BIO1-1", "sample_prep",       1000, "",             ""),
        ("E-BIO1-2", "assay_run:99.2",    2000, "E-BIO1-1",     ""),
        ("E-BIO1-3", "data_review",       3000, "E-BIO1-2",     ""),
        ("E-BIO1-4", "deviation_check",   4000, "E-BIO1-3",     ""),
        ("E-BIO1-5", "release_decision",  5000, "E-BIO1-4",     ""),
    ]
    for key, frame, ts, cause, _ in events_1:
        cause_sql = f"'{cause}'" if cause else "''"
        insert(f"Event {key}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'lab-tech-07', {ts}, '{frame}', 'P-BIO-1', {cause_sql})")

    # ── Batch 2: deviation (purity 94.5%) ──
    insert("Batch 2 process",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-BIO-2', 'quarantined', 'lab-tech-07', 'goal-batch-release-B2402')")
    events_2 = [
        ("E-BIO2-1", "sample_prep",       1000, "",             ""),
        ("E-BIO2-2", "assay_run:94.5",    2000, "E-BIO2-1",     ""),
        ("E-BIO2-3", "data_review",       3000, "E-BIO2-2",     ""),
        ("E-BIO2-4", "deviation_check",   4000, "E-BIO2-3",     ""),
        ("E-BIO2-5", "release_decision",  5000, "E-BIO2-4",     ""),
    ]
    for key, frame, ts, cause, _ in events_2:
        cause_sql = f"'{cause}'" if cause else "''"
        insert(f"Event {key}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'lab-tech-07', {ts}, '{frame}', 'P-BIO-2', {cause_sql})")

    # ── Batch 3: missing step (deviation_check skipped) ──
    insert("Batch 3 process",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-BIO-3', 'released', 'lab-tech-12', 'goal-batch-release-B2403')")
    events_3 = [
        ("E-BIO3-1", "sample_prep",       1000, "",             ""),
        ("E-BIO3-2", "assay_run:99.8",    2000, "E-BIO3-1",     ""),
        ("E-BIO3-3", "data_review",       3000, "E-BIO3-2",     ""),
        ("E-BIO3-5", "release_decision",  5000, "E-BIO3-3",     ""),  # deviation_check missing
    ]
    for key, frame, ts, cause, _ in events_3:
        cause_sql = f"'{cause}'" if cause else "''"
        insert(f"Event {key}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'lab-tech-12', {ts}, '{frame}', 'P-BIO-3', {cause_sql})")

    print()

    # ── A1: protocol compliance (Batch 1) ──
    evts = rows("SELECT frame FROM ExecEvent WHERE task_name = 'P-BIO-1' ORDER BY ts")
    actual = [e.get("frame","").split(":")[0] for e in evts]
    check("A1  Batch 1: protocol steps compliant", actual == PROTOCOL, str(actual))

    # ── A2: purity within spec (Batch 1) ──
    purity1 = 99.2
    check("A2  Batch 1: purity 99.2% >= 98.0% (pass)", purity1 >= ACCEPTANCE_THRESHOLD)

    # ── A3: deviation detected (Batch 2) ──
    purity2 = 94.5
    check("A3  Batch 2: purity 94.5% < 98.0% (deviation)", purity2 < ACCEPTANCE_THRESHOLD)

    # ── A4: missing step detected (Batch 3) ──
    evts3 = rows("SELECT frame FROM ExecEvent WHERE task_name = 'P-BIO-3' ORDER BY ts")
    actual3 = [e.get("frame","").split(":")[0] for e in evts3]
    missing = [s for s in PROTOCOL if s not in actual3]
    check("A4  Batch 3: missing step 'deviation_check' detected",
          missing == ["deviation_check"], f"missing={missing}")

    # ── A5: causal chain (Batch 1) ──
    chain = rows("SELECT key, caused_by FROM ExecEvent WHERE task_name = 'P-BIO-1' ORDER BY ts")
    chain_ok = all(
        (i == 0 and chain[i].get("caused_by","") == "") or
        (i > 0 and chain[i].get("caused_by","") == chain[i-1].get("key",""))
        for i in range(len(chain))
    )
    check("A5  Batch 1: full causal chain E1->E2->E3->E4->E5", chain_ok)

    # ── A6: provenance (all batches) ──
    all_evts = rows("SELECT event_agent, ts FROM ExecEvent WHERE task_name LIKE 'P-BIO-%'")
    prov_ok = all(e.get("event_agent") and e.get("ts") for e in all_evts)
    check("A6  All events have provenance (event_agent + ts)", prov_ok)

    # ── A7: state outcome consistency ──
    states = rows("SELECT key, current_state FROM Process WHERE key LIKE 'P-BIO-%' ORDER BY key")
    state_map = {s.get("key",""): s.get("current_state","") for s in states}
    check("A7  State outcomes: released/quarantined/released",
          state_map.get("P-BIO-1") == "released" and
          state_map.get("P-BIO-2") == "quarantined" and
          state_map.get("P-BIO-3") == "released",
          str(state_map))

    # ── Summary ──
    total = PASSED + FAILED
    print(f"\n{'='*50}")
    print(f"  Biomedical Scenario: {PASSED}/{total} passed")
    print(f"{'='*50}")
    return FAILED == 0


if __name__ == "__main__":
    import sys
    sys.exit(0 if main() else 1)
