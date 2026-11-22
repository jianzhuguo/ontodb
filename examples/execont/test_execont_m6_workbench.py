# -*- coding: utf-8 -*-
"""
ExecOnt M6 workbench validation: create a new scenario from zero.
Demonstrates: workbench flow (scenario -> DDL + events -> assertions -> run)
Acceptance: workbench enables new scenario in <= 1 day.
"""
import sys
import os
sys.path.insert(0, os.path.dirname(__file__))
from execont_workbench import Workbench


def main():
    wb = Workbench()

    # ── Define a brand-new scenario: Warehouse Logistics ──
    # (a scenario not used in M1-M5, created from scratch)
    scenario = {
        "name": "warehouse_logistics",
        "ontology": "exec_onto_wh",
        "classes": [
            {"name": "WhProcess", "properties": [
                {"name": "order_id", "range": "STRING"},
            ]},
            {"name": "WhActivity", "properties": [
                {"name": "zone", "range": "STRING"},
            ]},
            {"name": "WhEvent", "properties": [
                {"name": "item_count", "range": "FLOAT64"},
            ]},
        ],
        "process": {
            "key": "P-WH-001", "state": "running",
            "agent": "agv-07", "intent": "goal-pick-pack-ship",
        },
        "activities": [
            {"key": "A-WH-1", "name": "pick",   "process": "P-WH-001", "preceded_by": ""},
            {"key": "A-WH-2", "name": "pack",   "process": "P-WH-001", "preceded_by": "A-WH-1"},
            {"key": "A-WH-3", "name": "ship",   "process": "P-WH-001", "preceded_by": "A-WH-2"},
        ],
        "events": [
            {"key": "E-WH-1", "agent": "agv-07", "ts": 1000,
             "frame": "pick", "task": "P-WH-001", "caused_by": ""},
            {"key": "E-WH-2", "agent": "agv-07", "ts": 2000,
             "frame": "pack", "task": "P-WH-001", "caused_by": "E-WH-1"},
            {"key": "E-WH-3", "agent": "agv-07", "ts": 3000,
             "frame": "ship", "task": "P-WH-001", "caused_by": "E-WH-2"},
        ],
        "assertions": [
            {"label": "Process created",
             "query": "SELECT key FROM Process WHERE key = 'P-WH-001'",
             "expect_count": 1},
            {"label": "3 activities defined",
             "query": "SELECT activity_name FROM Activity WHERE process_of = 'P-WH-001'",
             "expect_count": 3},
            {"label": "3 events recorded",
             "query": "SELECT key FROM ExecEvent WHERE task_name = 'P-WH-001'",
             "expect_count": 3},
            {"label": "Causal chain E1->E2->E3",
             "query": "SELECT caused_by FROM ExecEvent WHERE key = 'E-WH-3'",
             "expect_key": "caused_by", "expect_value": "E-WH-2"},
            {"label": "Agent = agv-07",
             "query": "SELECT exec_agent FROM Process WHERE key = 'P-WH-001'",
             "expect_key": "exec_agent", "expect_value": "agv-07"},
            {"label": "State = running",
             "query": "SELECT current_state FROM Process WHERE key = 'P-WH-001'",
             "expect_key": "current_state", "expect_value": "running"},
            {"label": "Preceded_by chain valid",
             "query": "SELECT preceded_by FROM Activity WHERE key = 'A-WH-3'",
             "expect_key": "preceded_by", "expect_value": "A-WH-2"},
        ],
    }

    # ── Run through workbench ──
    result = wb.run(scenario)
    # Idempotency pin: re-running the same scenario must converge to the same
    # state (same business keys overwrite) instead of accumulating rows.
    rerun = wb.run(scenario)

    total = rerun["passed"] + rerun["failed"]
    print(f"\n{'='*50}")
    print(f"  Workbench validation: {rerun['passed']}/{total} passed")
    if result["failed"] == 0 and rerun["failed"] == 0:
        print("  NEW SCENARIO CREATED AND VERIFIED FROM ZERO")
        print("  RE-RUN IDEMPOTENT (second execution also clean)")
    print(f"{'='*50}")
    return result["failed"] == 0 and rerun["failed"] == 0


if __name__ == "__main__":
    sys.exit(0 if main() else 1)
