import urllib.request
import json
import sys

API = 'http://127.0.0.1:7912/api/query'

def execute(sql):
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"):
                print(f"  ERROR: {result['error'][:100]}")
            else:
                msg = result.get("data", {})
                if isinstance(msg, dict) and msg.get("message"):
                    print(f"  OK: {msg['message']}")
                elif isinstance(msg, list):
                    print(f"  OK: {len(msg)} rows")
                else:
                    print(f"  OK")
    except Exception as e:
        print(f"  FAIL: {e}")

with open(r"E:\ontodb\test_data\enterprise_seed.sql", encoding="utf-8") as f:
    content = f.read()

statements = []
for line in content.split('\n'):
    line = line.strip()
    if not line or line.startswith('--'):
        continue
    statements.append(line)

print(f"Executing {len(statements)} SQL statements...")
for i, sql in enumerate(statements):
    print(f"[{i+1}/{len(statements)}] {sql[:60]}...")
    execute(sql)

print("\nDone! Verifying data...")
for cls in ['Department', 'Employee', 'Project', 'Server', 'BizMetrics', 'Alert']:
    execute(f"SELECT * FROM {cls}")
