import urllib.request, json

API = 'http://127.0.0.1:7912/api/query'

def q(sql):
    d = json.dumps({'query': sql}).encode()
    r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(r, timeout=10) as resp:
        return len(json.loads(resp.read()).get('data', []))

tables = ['Department', 'Employee', 'Project', 'Server', 'Customer', 'Lead', 'BizMetrics', 'Alert', 'Decision', 'DecisionMaker']
for t in tables:
    try:
        count = q('SELECT * FROM ' + t)
        print(f'  {t:20s}: {count} rows')
    except Exception as e:
        print(f'  {t:20s}: error - {e}')
