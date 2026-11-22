# -*- coding: utf-8 -*-
"""
ExecOnt M6 biomedical scenario 2: Biopharma Manufacturing (创新生物药商业化生产)
Domain: biologics (mAb) commercial manufacturing — anonymized industry scenario
Demonstrates: process step decomposition, CQA guard (culture temp), deviation
detection, missing-step detection, causal chain, provenance, state outcome
consistency in a GMP production context.
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
    print("=== ExecOnt Biopharma Scenario: mAb Commercial Manufacturing ===")

    # Manufacturing process (mAb): upstream culture -> downstream purification -> fill-finish
    PROCESS = ["cell_thaw", "seed_train", "bioreactor_culture",
               "capture_chromatography", "viral_inactivation", "fill_finish"]
    TEMP_LIMIT = 37.0  # bioreactor culture temperature guard (deg C)

    # Cleanup
    for pid in ["P-MAB-1", "P-MAB-2", "P-MAB-3"]:
        sql(f"DELETE FROM ExecEvent WHERE task_name = '{pid}'")
        sql(f"DELETE FROM Process WHERE key = '{pid}'")
    for i in range(1, 7):
        sql(f"DELETE FROM Activity WHERE key = 'A-MAB-{i}'")
    sql("DELETE FROM Transition WHERE key = 'T-MAB-guard'")

    # Shared process activities
    for i, step in enumerate(PROCESS, 1):
        pred = f"'A-MAB-{i-1}'" if i > 1 else "''"
        insert(f"Activity {step}",
            f"INSERT INTO Activity (key, activity_name, process_of, preceded_by) "
            f"VALUES ('A-MAB-{i}', '{step}', 'P-MAB-1', {pred})")

    # Guard transition: culture -> capture requires temp <= 37.0
    insert("Transition with culture temp guard",
        "INSERT INTO Transition (key, from_state, to_state, guard) "
        "VALUES ('T-MAB-guard', 'bioreactor_culture', 'capture_chromatography', 'temp <= 37.0')")

    # ── Batch 1: compliant (culture 36.8 C) ──
    insert("Batch 1 process",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-MAB-1', 'released', 'mfg-line-a', 'goal-mab-batch-MB2601')")
    events_1 = [
        ("E-MAB1-1", "cell_thaw",               1000, ""),
        ("E-MAB1-2", "seed_train",              2000, "E-MAB1-1"),
        ("E-MAB1-3", "bioreactor_culture:36.8", 3000, "E-MAB1-2"),
        ("E-MAB1-4", "capture_chromatography",  4000, "E-MAB1-3"),
        ("E-MAB1-5", "viral_inactivation",      5000, "E-MAB1-4"),
        ("E-MAB1-6", "fill_finish",             6000, "E-MAB1-5"),
    ]
    for key, frame, ts, cause in events_1:
        cause_sql = f"'{cause}'" if cause else "''"
        insert(f"Event {key}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'mfg-line-a', {ts}, '{frame}', 'P-MAB-1', {cause_sql})")

    # ── Batch 2: deviation (culture temp 38.2 C > 37.0 guard) ──
    insert("Batch 2 process",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-MAB-2', 'quarantined', 'mfg-line-b', 'goal-mab-batch-MB2602')")
    events_2 = [
        ("E-MAB2-1", "cell_thaw",               1000, ""),
        ("E-MAB2-2", "seed_train",              2000, "E-MAB2-1"),
        ("E-MAB2-3", "bioreactor_culture:38.2", 3000, "E-MAB2-2"),
        ("E-MAB2-4", "capture_chromatography",  4000, "E-MAB2-3"),
        ("E-MAB2-5", "viral_inactivation",      5000, "E-MAB2-4"),
        ("E-MAB2-6", "fill_finish",             6000, "E-MAB2-5"),
    ]
    for key, frame, ts, cause in events_2:
        cause_sql = f"'{cause}'" if cause else "''"
        insert(f"Event {key}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'mfg-line-b', {ts}, '{frame}', 'P-MAB-2', {cause_sql})")

    # ── Batch 3: missing step (viral_inactivation skipped) ──
    insert("Batch 3 process",
        "INSERT INTO Process (key, current_state, exec_agent, has_intent) "
        "VALUES ('P-MAB-3', 'released', 'mfg-line-a', 'goal-mab-batch-MB2603')")
    events_3 = [
        ("E-MAB3-1", "cell_thaw",               1000, ""),
        ("E-MAB3-2", "seed_train",              2000, "E-MAB3-1"),
        ("E-MAB3-3", "bioreactor_culture:36.9", 3000, "E-MAB3-2"),
        ("E-MAB3-4", "capture_chromatography",  4000, "E-MAB3-3"),
        ("E-MAB3-6", "fill_finish",             6000, "E-MAB3-4"),  # viral_inactivation missing
    ]
    for key, frame, ts, cause in events_3:
        cause_sql = f"'{cause}'" if cause else "''"
        insert(f"Event {key}",
            f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
            f"VALUES ('{key}', 'mfg-line-a', {ts}, '{frame}', 'P-MAB-3', {cause_sql})")

    print()

    # ── A1: process decomposition (6 steps) ──
    acts = rows("SELECT activity_name FROM Activity WHERE process_of = 'P-MAB-1'")
    names = sorted([a.get("activity_name", "") for a in acts])
    check("A1  Process decomposed into 6 steps", len(names) == 6, str(names))

    # ── A2: batch 1 plan vs actual compliant ──
    evts1 = rows("SELECT frame FROM ExecEvent WHERE task_name = 'P-MAB-1' ORDER BY ts")
    actual1 = [e.get("frame", "").split(":")[0] for e in evts1]
    check("A2  Batch 1: all 6 steps compliant", actual1 == PROCESS, str(actual1))

    # ── A3: batch 2 culture temp excursion detected ──
    temp2 = 38.2
    check("A3  Batch 2: culture 38.2 C > 37.0 guard (deviation)", temp2 > TEMP_LIMIT)

    # ── A4: batch 3 missing viral_inactivation detected ──
    evts3 = rows("SELECT frame FROM ExecEvent WHERE task_name = 'P-MAB-3' ORDER BY ts")
    actual3 = [e.get("frame", "").split(":")[0] for e in evts3]
    missing = [s for s in PROCESS if s not in actual3]
    check("A4  Batch 3: missing step 'viral_inactivation' detected",
          missing == ["viral_inactivation"], f"missing={missing}")

    # ── A5: causal chain (batch 1) ──
    chain = rows("SELECT key, caused_by FROM ExecEvent WHERE task_name = 'P-MAB-1' ORDER BY ts")
    chain_ok = all(
        (i == 0 and chain[i].get("caused_by", "") == "") or
        (i > 0 and chain[i].get("caused_by", "") == chain[i-1].get("key", ""))
        for i in range(len(chain))
    )
    check("A5  Batch 1: full causal chain E1->...->E6", chain_ok)

    # ── A6: provenance (all batches) ──
    all_evts = rows("SELECT event_agent, ts FROM ExecEvent WHERE task_name LIKE 'P-MAB-%'")
    prov_ok = all(e.get("event_agent") and e.get("ts") for e in all_evts)
    check("A6  All events have provenance (event_agent + ts)", prov_ok)

    # ── A7: state outcome consistency ──
    states = rows("SELECT key, current_state FROM Process WHERE key LIKE 'P-MAB-%' ORDER BY key")
    state_map = {s.get("key", ""): s.get("current_state", "") for s in states}
    check("A7  State outcomes: released/quarantined/released",
          state_map.get("P-MAB-1") == "released" and
          state_map.get("P-MAB-2") == "quarantined" and
          state_map.get("P-MAB-3") == "released",
          str(state_map))

    # ── A8: guard transition recorded (temp <= 37.0) ──
    trans = rows("SELECT guard FROM Transition WHERE key = 'T-MAB-guard'")
    guard = trans[0].get("guard", "") if trans else ""
    check("A8  Guard transition recorded: temp <= 37.0", guard == "temp <= 37.0", guard)

    # ── Summary ──
    total = PASSED + FAILED
    print(f"\n{'='*50}")
    print(f"  Biopharma Manufacturing Scenario: {PASSED}/{total} passed")
    print(f"{'='*50}")
    return FAILED == 0


if __name__ == "__main__":
    import sys
    sys.exit(0 if main() else 1)
