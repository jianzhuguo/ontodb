import requests
import json

# Get schema to find ontology keys
resp = requests.get('http://localhost:7912/api/schema')
schema = resp.json()

# Find ontology names
ontologies = schema['data']['ontologies']
print('Found ontologies:', [o['name'] for o in ontologies])

# Try to delete each ontology
for onto in ontologies:
    name = onto['name']
    if name in ['ValueHub', 'MimoCode', 'BioCompute']:
        # Try different delete approaches
        queries = [
            f'DELETE FROM "__ontology__{name}"',
            f'DELETE FROM "__ontology___default::{name}"',
        ]
        for q in queries:
            try:
                resp = requests.post('http://localhost:7912/api/query', json={'query': q})
                result = resp.json()
                print(f'{q} -> {result}')
            except Exception as e:
                print(f'{q} -> Error: {e}')
