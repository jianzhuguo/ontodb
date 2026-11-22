# -*- coding: utf-8 -*-
"""
ExecOnt Modeling Workbench (建模工作台)
Spec: docs/执行态本体开发方案与战略意义.md §6 M6
Flow: scenario definition -> DDL + event samples -> assertion suite -> run
Usage:
    from execont_workbench import Workbench
    wb = Workbench()
    result = wb.run(scenario_dict)
"""
import json
import urllib.request


class Workbench:
    """Scenario-to-assertion workbench for ExecOnt."""

    def __init__(self, base_url="http://127.0.0.1:7912/api/query"):
        self.base_url = base_url

    # ── SQL execution ──────────────────────────────────────
    def sql(self, query: str) -> dict:
        body = json.dumps({"query": query}).encode("utf-8")
        req = urllib.request.Request(
            self.base_url, data=body,
            headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            return {"success": False,
                    "error": e.read().decode("utf-8", errors="replace")}

    def rows(self, query: str) -> list:
        return self.sql(query).get("data", []) or []

    # ── DDL generation ─────────────────────────────────────
    @staticmethod
    def generate_ddl(scenario: dict) -> str:
        """Generate CREATE ONTOLOGY from scenario class/property definitions.

        scenario = {
            "ontology": "my_onto",
            "classes": [
                {"name": "MyClass", "extends": None,
                 "properties": [
                    {"name": "prop1", "range": "STRING", "required": False},
                 ]},
            ],
        }
        """
        onto_name = scenario.get("ontology", "exec_onto_gen")
        parts = []
        for cls in scenario.get("classes", []):
            cls_line = f"  CLASS {cls['name']}"
            if cls.get("extends"):
                cls_line += f" SUBCLASS OF {cls['extends']}"
            parts.append(cls_line)
        for cls in scenario.get("classes", []):
            for prop in cls.get("properties", []):
                rng = prop.get("range", "STRING")
                parts.append(
                    f"  PROPERTY {prop['name']} DOMAIN {cls['name']} RANGE {rng}")
        ddl = f"CREATE ONTOLOGY {onto_name} (\n" + ",\n".join(parts) + "\n)"
        return ddl

    # ── Event sample generation ────────────────────────────
    @staticmethod
    def generate_inserts(scenario: dict) -> list:
        """Generate INSERT statements from scenario data definitions.

        scenario = {
            "process": {"key": "P-001", "state": "running", "agent": "a-01", "intent": "g-1"},
            "activities": [
                {"key": "A-1", "name": "step1", "process": "P-001", "preceded_by": ""},
            ],
            "events": [
                {"key": "E-1", "agent": "a-01", "ts": 1000,
                 "frame": "step1", "task": "P-001", "caused_by": ""},
            ],
        }
        """
        stmts = []
        proc = scenario.get("process", {})
        if proc:
            stmts.append(
                f"INSERT INTO Process (key, current_state, exec_agent, has_intent) "
                f"VALUES ('{proc['key']}', '{proc.get('state','running')}', "
                f"'{proc.get('agent','')}', '{proc.get('intent','')}')")
        for act in scenario.get("activities", []):
            pred = f"'{act['preceded_by']}'" if act.get("preceded_by") else "''"
            stmts.append(
                f"INSERT INTO Activity (key, activity_name, process_of, preceded_by) "
                f"VALUES ('{act['key']}', '{act['name']}', "
                f"'{act['process']}', {pred})")
        for ev in scenario.get("events", []):
            cause = f"'{ev['caused_by']}'" if ev.get("caused_by") else "''"
            stmts.append(
                f"INSERT INTO ExecEvent (key, event_agent, ts, frame, task_name, caused_by) "
                f"VALUES ('{ev['key']}', '{ev['agent']}', {ev['ts']}, "
                f"'{ev['frame']}', '{ev['task']}', {cause})")
        return stmts

    # ── Assertion runner ───────────────────────────────────
    def run_assertions(self, assertions: list) -> list:
        """Run a list of assertion dicts and return results.

        Assertion types:
          {"label": "...", "query": "...", "expect_count": N}
          {"label": "...", "query": "...", "expect_values": [...]}
          {"label": "...", "query": "...", "expect_contains": "..."}
        """
        results = []
        for a in assertions:
            label = a.get("label", "unnamed")
            query = a.get("query", "")
            r = self.sql(query)
            data = r.get("data", []) or []
            ok = False
            detail = ""

            if "expect_count" in a:
                ok = len(data) == a["expect_count"]
                detail = f"got {len(data)}, want {a['expect_count']}"
            elif "expect_values" in a:
                got = [row for row in data]
                ok = got == a["expect_values"]
                detail = f"got {got}"
            elif "expect_contains" in a:
                text = json.dumps(data)
                ok = a["expect_contains"] in text
                detail = f"'{a['expect_contains']}' in {text[:80]}"
            elif "expect_key" in a:
                key = a["expect_key"]
                val = a.get("expect_value", "")
                got_val = data[0].get(key, "") if data else ""
                ok = str(got_val) == str(val)
                detail = f"got {got_val}, want {val}"
            else:
                ok = bool(data)
                detail = f"{len(data)} rows"

            results.append({"label": label, "ok": ok, "detail": detail})
        return results

    # ── Full scenario runner ───────────────────────────────
    def run(self, scenario: dict) -> dict:
        """Execute a full scenario: DDL -> setup -> assertions -> report.

        Returns {"passed": N, "failed": N, "results": [...]}
        """
        name = scenario.get("name", "unnamed")
        print(f"--- Workbench scenario: {name} ---")

        # 1. DDL (optional)
        if scenario.get("classes"):
            ddl = self.generate_ddl(scenario)
            r = self.sql(ddl)
            status = "OK" if r.get("success") else f"SKIP ({r.get('error','')[:60]})"
            print(f"  DDL: {status}")

        # 2. Setup
        stmts = self.generate_inserts(scenario)
        for s in stmts:
            r = self.sql(s)
            if not r.get("success"):
                print(f"  [WARN] setup: {r.get('error','')[:80]}")

        # 3. Assertions
        results = self.run_assertions(scenario.get("assertions", []))
        passed = sum(1 for r in results if r["ok"])
        failed = sum(1 for r in results if not r["ok"])
        for r in results:
            tag = "PASS" if r["ok"] else "FAIL"
            extra = f"  ({r['detail']})" if not r["ok"] else ""
            print(f"  [{tag}] {r['label']}{extra}")

        print(f"  Result: {passed}/{passed+failed} passed")
        return {"passed": passed, "failed": failed, "results": results}
