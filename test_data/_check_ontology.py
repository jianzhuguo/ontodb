import urllib.request, json

API = 'http://127.0.0.1:7912/api/query'

queries = [
    'SELECT * FROM __ontologies__',
    'SELECT * FROM __ontology_classes__',
    'SELECT * FROM __ontology_properties__',
    'SELECT * FROM __owl_subclass__',
    'SELECT * FROM __rdfs_subproperty__',
    'SELECT * FROM __rdf_type__',
    'SHOW TABLES',
]

for sql in queries:
    try:
        d = json.dumps({'query': sql}).encode()
        r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(r, timeout=5) as resp:
            result = json.loads(resp.read())
            if result.get('error'):
                print(f'  {sql:50s} -> ERR: {result["error"][:80]}')
            else:
                rows = result.get('data', [])
                print(f'  {sql:50s} -> {len(rows)} rows')
                if rows and len(rows) <= 5:
                    for row in rows[:3]:
                        print(f'    {json.dumps(row, ensure_ascii=False)[:120]}')
    except Exception as e:
        print(f'  {sql:50s} -> {e}')

# Also check /api/schema
try:
    r = urllib.request.Request('http://127.0.0.1:7912/api/schema')
    with urllib.request.urlopen(r, timeout=5) as resp:
        result = json.loads(resp.read())
        print(f'\n/api/schema: {json.dumps(result, ensure_ascii=False)[:500]}')
except Exception as e:
    print(f'/api/schema: {e}')
