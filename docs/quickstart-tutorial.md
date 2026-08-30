# OntoDB 蹇€熷叆闂ㄦ暀绋?
> 10 鍒嗛挓瀛︿細 OntoDB 鐨勬牳蹇冨姛鑳?
---

## 绗?1 姝ワ細鍚姩鏈嶅姟鍣?
```bash
# 涓嬭浇骞惰В鍘?wget https://release.ontovalue.com/ontodb-v0.6.2-linux-x86_64.tar.gz
tar xzf ontodb-v0.6.2-linux-x86_64.tar.gz
cd ontodb-v0.6.2

# 鍚姩
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --no-rate-limit
```

鐪嬪埌 `HTTP API server listening on 127.0.0.1:7912` 琛ㄧず鍚姩鎴愬姛銆?
---

## 绗?2 姝ワ細鍒涘缓琛ㄥ苟鎻掑叆鏁版嵁

```bash
# 鍒涘缓鐢ㄦ埛绫伙紙OntoQL 璇硶锛?curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE CLASS users"}'

# 鎻掑叆鏁版嵁锛圫ET 璇硶锛?curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users SET name = \"Alice\", age = 30, city = \"鍖椾含\""}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users SET name = \"Bob\", age = 25, city = \"涓婃捣\""}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users SET name = \"Charlie\", age = 35, city = \"鍖椾含\""}'
```

---

## 绗?3 姝ワ細鏌ヨ鏁版嵁

```bash
# 鏌ヨ鎵€鏈夌敤鎴?curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users"}'

# 鏉′欢鏌ヨ
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users WHERE age > 28"}'

# 鑱氬悎鏌ヨ
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT city, COUNT(*) as count FROM users GROUP BY city"}'
```

---

## 绗?4 姝ワ細鍚戦噺鎼滅储

```bash
# 鍒涘缓鏂囨。绫?curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE CLASS documents"}'

# 鎻掑叆甯﹀悜閲忕殑鏂囨。
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO documents SET title = \"Rust鍏ラ棬\", content = \"Rust鏄郴缁熺紪绋嬭瑷€\", embedding = [0.1, 0.2, 0.3, 0.4, 0.5]"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO documents (title, content, embedding) VALUES (\"Python鏁欑▼\", \"Python鏄剼鏈瑷€\", [0.2, 0.3, 0.4, 0.5, 0.6])"}'

# 鍚戦噺鎼滅储锛堟壘鏈€鐩镐技鐨勬枃妗ｏ級
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "documents",
    "column": "embedding",
    "query_vector": [0.15, 0.25, 0.35, 0.45, 0.55],
    "top_k": 2
  }'
```

---

## 绗?5 姝ワ細鍥炬煡璇?
```bash
# 鍒涘缓浜虹墿绫?curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE CLASS Person"}'

# 鎻掑叆浜虹墿
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Person SET name = \"Alice\", age = 30"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Person (name, age) VALUES (\"Bob\", 25)"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Person (name, age) VALUES (\"Charlie\", 35)"}'

# 鎻掑叆鍏崇郴
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO knows (from_id, to_id) VALUES (\"Person::1\", \"Person::2\")"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO knows (from_id, to_id) VALUES (\"Person::2\", \"Person::3\")"}'

# 鍥鹃亶鍘嗭紙浠?Alice 鍑哄彂锛? 璺筹級
curl -X POST http://127.0.0.1:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -d '{
    "start": "Person::1",
    "direction": "out",
    "max_depth": 2
  }'
```

---

## 绗?6 姝ワ細浣跨敤 Python SDK

```bash
pip install ontodb
```

```python
from ontodb import OntoDB

# 杩炴帴
db = OntoDB("http://localhost:7912")

# 鏌ヨ
rows = db.query("SELECT * FROM users")
for row in rows:
    print(f"{row['name']}: {row['age']}宀? {row['city']}")

# 鍚戦噺鎼滅储
results = db.vector_search("documents", "embedding", [0.15, 0.25, 0.35, 0.45, 0.55], top_k=2)
for r in results:
    print(f"{r['title']} (鐩镐技搴? {r.get('_score', 'N/A')})")

# 鎵归噺鎻掑叆
db.insert_many("users", [
    {"name": "David", "age": 28, "city": "娣卞湷"},
    {"name": "Eve", "age": 32, "city": "鏉窞"},
])
```

---

## 绗?7 姝ワ細鏌ョ湅鐩戞帶

```bash
# 鍋ュ悍妫€鏌?curl http://127.0.0.1:7912/api/health

# 鑾峰彇鎸囨爣
curl http://127.0.0.1:7912/api/metrics

# Prometheus 鏍煎紡鎸囨爣
curl http://127.0.0.1:7912/metrics

# 鎵撳紑 Web 鎺у埗鍙?# 娴忚鍣ㄨ闂?http://127.0.0.1:7912/console
```

---

## 涓嬩竴姝?
- 闃呰 [鐢ㄦ埛鎵嬪唽](user-manual.md) 浜嗚В鏇村鍔熻兘
- 鏌ョ湅 [API 鏂囨。](http://127.0.0.1:7912/api/docs) 浜嗚В鎵€鏈夋帴鍙?- 鎺㈢储 [鏁板瓧鍐涘笀](/digital-advisor) 鍐崇瓥鏅鸿兘绯荤粺
- 鎺㈢储 [鏁板瓧瀛敓](/digital-twin) 鐩戞帶澶у睆

---

## 甯歌闂

**Q: 濡備綍淇敼绔彛锛?*
```bash
./ontodb-server --http 0.0.0.0:8080
```

**Q: 濡備綍鍚敤璁よ瘉锛?*
```bash
./ontodb-server --auth --api-key my-secret-key
```

**Q: 鏁版嵁瀛樺偍鍦ㄥ摢閲岋紵**
榛樿鍦?`--data-dir` 鎸囧畾鐨勭洰褰曪紝閫氬父鏄?`./data/`

**Q: 濡備綍澶囦唤锛?*
```bash
curl -X POST http://127.0.0.1:7912/api/backup \
  -H "Content-Type: application/json" \
  -d '{"path": "/backups/my-backup.ontodb"}'
```

**Q: 鏀寔鍝簺瀹㈡埛绔紵**
- HTTP REST API锛堜换浣曡瑷€锛?- PostgreSQL 瀹㈡埛绔紙psql銆乸gAdmin 绛夛級
- MySQL 瀹㈡埛绔紙mysql銆丮ySQL Workbench 绛夛級
- Python SDK銆丣avaScript SDK銆丟o SDK銆丣ava SDK
