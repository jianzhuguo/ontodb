import requests
import json

base_url = "http://127.0.0.1:7912/api/query"

# First, let's try to get a sample of keys to understand the key structure
print("=== Sample Drug PKs ===")
resp = requests.post(base_url, json={"query": "SELECT __pk__, __class__ FROM 'sembio.Drug' LIMIT 3"})
data = resp.json()
if data["success"]:
    for row in data["data"]:
        print("PK:", row['__pk__'])
        print("Class:", row['__class__'])

# Now let's try to find all distinct prefixes by scanning with a very broad filter
print("\n=== Scanning for all distinct prefixes ===")

# Try to get all unique __class__ values from all data
resp = requests.post(base_url, json={"query": "SELECT DISTINCT __class__ FROM 'sembio.Drug' LIMIT 100"})
data = resp.json()
if data["success"]:
    print("Distinct classes in Drug:", len(data['data']))
    for row in data["data"]:
        print("  ", row['__class__'])

# Try to scan with different prefixes to find all data
prefixes = [
    "sembio",
    "compound",
    "compoundproperty",
    "protein",
    "gene",
    "drug",
    "disease",
    "pathway",
    "interaction",
    "relation",
    "property",
    "structure",
    "sequence",
    "annotation",
    "metadata",
    "index",
    "cache",
    "temp",
    "test",
    "sample",
    "example"
]

print("\n=== Scanning for data with different prefixes ===")
for prefix in prefixes:
    try:
        resp = requests.post(base_url, json={"query": "SELECT COUNT(*) as cnt FROM '" + prefix + "' LIMIT 1"})
        data = resp.json()
        if data["success"] and data["data"][0]["cnt"] > 0:
            print("Prefix '" + prefix + "':", data['data'][0]['cnt'], "records")
    except:
        pass

# Try to get all data from the storage by scanning with empty prefix
print("\n=== Scanning for all data with empty prefix ===")
resp = requests.post(base_url, json={"query": "SELECT __class__, COUNT(*) as cnt FROM 'all' GROUP BY __class__ ORDER BY cnt DESC LIMIT 50"})
data = resp.json()
if data["success"]:
    print("Total classes found:", len(data['data']))
    for row in data["data"]:
        print(row['__class__'], ":", row['cnt'])
else:
    print("Error:", data['error'])

# Try to get all data from the storage by scanning with wildcard
print("\n=== Scanning for all data with wildcard ===")
resp = requests.post(base_url, json={"query": "SELECT __class__, COUNT(*) as cnt FROM '*' GROUP BY __class__ ORDER BY cnt DESC LIMIT 50"})
data = resp.json()
if data["success"]:
    print("Total classes found:", len(data['data']))
    for row in data["data"]:
        print(row['__class__'], ":", row['cnt'])
else:
    print("Error:", data['error'])
