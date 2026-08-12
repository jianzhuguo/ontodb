import urllib.request, json, time

API = 'http://127.0.0.1:7912/api/query'

def execute(sql):
    time.sleep(0.05)
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"):
                print(f"  ERR: {result['error'][:100]}")
                return False
            return True
    except Exception as e:
        print(f"  FAIL: {e}")
        return False

# Read SQL file
with open(r"E:\ontodb\test_data\digital_advisor_seed.sql", encoding="utf-8") as f:
    content = f.read()

# Split into statements
stmts = []
for line in content.split('\n'):
    line = line.strip()
    if not line or line.startswith('--') or line.startswith('═'):
        continue
    stmts.append(line)

ok = fail = 0
for i, sql in enumerate(stmts):
    if execute(sql):
        ok += 1
    else:
        fail += 1
    if (i+1) % 10 == 0:
        print(f"  [{i+1}/{len(stmts)}] processed...")

print(f"\nDone: {ok} OK, {fail} failed")

# Verify
print("\n--- 验证 ---")
for cls in ['DecisionMaker', 'Decision', 'DecisionContext', 'DecisionOutcome', 'DecisionStep', 'PersonalityTrait']:
    def q(sql):
        d = json.dumps({'query': sql}).encode()
        r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(r, timeout=10) as resp:
            return len(json.loads(resp.read()).get('data', []))
    print(f"  {cls:20s}: {q('SELECT * FROM ' + cls)} rows")
