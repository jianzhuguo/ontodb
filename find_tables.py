import requests
import json

base_url = "http://127.0.0.1:7912/api/query"

# Try querying without sembio. prefix
tables_to_try = [
    "CompoundProperty",
    "Compound",
    "compoundproperty",
    "compound",
    "BioAssay",
    "sembio.BioAssay",
    "Drug",
    "Protein",
    "Gene",
]

for table in tables_to_try:
    try:
        resp = requests.post(base_url, json={"query": "SELECT COUNT(*) as cnt FROM \"" + table + "\""})
        data = resp.json()
        if data["success"]:
            cnt = data["data"][0]["cnt"]
            if cnt > 0:
                print(table + " : " + str(cnt))
            else:
                print(table + " : 0")
        else:
            print(table + " : ERROR - " + data.get("error", "unknown"))
    except Exception as e:
        print(table + " : EXCEPTION - " + str(e))

# Also try some common bioinformatics table names
extra = [
    "CompoundProperty", "CompoundStructure", "ProteinSequence",
    "DrugTarget", "GeneExpression", "PathwayEnrichment",
    "ProteinInteraction", "MolecularFeature", "Activity",
    "Target", "Assay", "CellLine", "Mechanism",
    "CompoundProperty", "DrugIndication", "DrugSideEffect",
    "ProteinAnnotation", "GeneAnnotation", "VariantAnnotation",
    "DiseaseAnnotation", "MetaboliteAnnotation"
]

print("\n=== Extra tables ===")
for table in extra:
    try:
        resp = requests.post(base_url, json={"query": "SELECT COUNT(*) as cnt FROM \"" + table + "\""})
        data = resp.json()
        if data["success"]:
            cnt = data["data"][0]["cnt"]
            if cnt > 0:
                print(table + " : " + str(cnt))
    except:
        pass
