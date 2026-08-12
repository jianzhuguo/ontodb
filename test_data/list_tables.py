import urllib.request, json

API = 'http://127.0.0.1:7912/api/query'

def q(sql):
    d = json.dumps({'query': sql}).encode()
    r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(r, timeout=10) as resp:
        return json.loads(resp.read()).get('data', [])

# 查询所有表的数据量
tables = [
    'Department', 'Employee', 'Project', 'Server', 'Customer', 'Lead',
    'BizMetrics', 'Alert', 'Decision', 'DecisionMaker', 'DecisionContext',
    'DecisionOutcome', 'DecisionStep', 'PersonalityTrait'
]

print('=' * 50)
print('  OntoDB 数据库表清单')
print('=' * 50)
total = 0
for t in tables:
    try:
        rows = q('SELECT * FROM ' + t)
        count = len(rows)
        total += count
        print(f'  {t:25s} {count:>5} 行')
    except:
        print(f'  {t:25s}   N/A')

print('=' * 50)
print(f'  总计: {total} 行')
print('=' * 50)
