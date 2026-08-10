import urllib.request, json
API = 'http://127.0.0.1:7912/api/query'
def q(sql):
    d = json.dumps({'query': sql}).encode()
    r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(r, timeout=10) as resp:
        return len(json.loads(resp.read()).get('data', []))
for t in ['Department','Employee','Project','Server','Customer','Lead','Alert','BizMetrics']:
    print(f'  {t:15s}: {q("SELECT * FROM " + t)} rows')
