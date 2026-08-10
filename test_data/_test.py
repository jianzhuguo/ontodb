import urllib.request
import json
import time

API = 'http://127.0.0.1:7912/api/query'

def execute(sql, delay=0.15):
    time.sleep(delay)
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"):
                print(f"  ERR: {result['error'][:120]}")
                return False
            else:
                print(f"  OK")
                return True
    except urllib.error.HTTPError as e:
        body = e.read().decode('utf-8', errors='replace')
        print(f"  HTTP {e.code}: {body[:120]}")
        return False
    except Exception as e:
        print(f"  FAIL: {e}")
        return False

# Test with one statement first
print("Testing CREATE CLASS...")
execute("CREATE CLASS Department (name STRING, code STRING, head STRING, budget DOUBLE, level INT, parent STRING)")

print("\nTesting INSERT...")
execute("INSERT INTO Department VALUES ('集团总部', 'HQ', '张明远', 50000000, 1, '')")

print("\nTesting SELECT...")
execute("SELECT * FROM Department")
