# OntoDB 鐢ㄦ埛鎵嬪唽

> 鐗堟湰锛歷0.6.2 | 鏇存柊鏃ユ湡锛?026-08-27

---

## 鐩綍

1. [蹇€熷叆闂╙(#1-蹇€熷叆闂?
2. [瀹夎閮ㄧ讲](#2-瀹夎閮ㄧ讲)
3. [SQL 璇硶](#3-sql-璇硶)
4. [鍚戦噺鎼滅储](#4-鍚戦噺鎼滅储)
5. [鍥炬煡璇(#5-鍥炬煡璇?
6. [SPARQL 鏌ヨ](#6-sparql-鏌ヨ)
7. [鏈綋鎺ㄧ悊](#7-鏈綋鎺ㄧ悊)
8. [浜嬪姟绠＄悊](#8-浜嬪姟绠＄悊)
9. [澶囦唤鎭㈠](#9-澶囦唤鎭㈠)
10. [瀹夊叏閰嶇疆](#10-瀹夊叏閰嶇疆)
11. [鎬ц兘璋冧紭](#11-鎬ц兘璋冧紭)
12. [鏁呴殰鎺掓煡](#12-鏁呴殰鎺掓煡)

---

## 1. 蹇€熷叆闂?

### 1.1 涓夊垎閽熶笂鎵?

```bash
# 1. 涓嬭浇骞惰В鍘?
wget https://release.ontovalue.com/ontodb-v0.6.2-linux-x86_64.tar.gz
tar xzf ontodb-v0.6.2-linux-x86_64.tar.gz

# 2. 鍚姩鏈嶅姟鍣?
./ontodb-server --data-dir ./data --http 127.0.0.1:7912

# 3. 鍒涘缓琛ㄥ苟鎻掑叆鏁版嵁锛圤ntoQL 璇硶锛?
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE CLASS users"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users SET name = \"Alice\", age = 30"}'

# 4. 鏌ヨ鏁版嵁
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users"}'
```

### 1.2 浣跨敤 CLI

```bash
# 杩炴帴鍒版湇鍔″櫒
./ontodb-cli 127.0.0.1:7912

# 浜や簰寮忔煡璇?
ontodb> SELECT * FROM users;
鈹屸攢鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹攢鈹€鈹€鈹€鈹€鈹?
鈹?name    鈹?age 鈹?
鈹溾攢鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹尖攢鈹€鈹€鈹€鈹€鈹?
鈹?Alice   鈹?30  鈹?
鈹斺攢鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹粹攢鈹€鈹€鈹€鈹€鈹?
(1 row)
```

### 1.3 浣跨敤 Web 鎺у埗鍙?

鎵撳紑娴忚鍣ㄨ闂?`http://127.0.0.1:7912/console`锛屾敮鎸侊細
- SQL 缂栬緫鍣紙璇硶楂樹寒锛?
- 鏌ヨ鍘嗗彶
- Schema 娴忚
- 瀹炴椂鎸囨爣

---

## 2. 瀹夎閮ㄧ讲

### 2.1 绯荤粺瑕佹眰

| 椤圭洰 | 鏈€浣庤姹?| 鎺ㄨ崘閰嶇疆 |
|------|---------|---------|
| CPU | 2 鏍?| 8 鏍?|
| 鍐呭瓨 | 2 GB | 16 GB |
| 纾佺洏 | 10 GB SSD | 100 GB NVMe SSD |
| 鎿嶄綔绯荤粺 | Linux x86_64 / Windows 10+ | Ubuntu 22.04 / Windows 11 |

### 2.2 Linux 瀹夎

```bash
# 涓€閿畨瑁?
curl -fsSL https://get.ontovalue.com/install.sh | bash

# 鎴栨墜鍔ㄥ畨瑁?
wget https://release.ontovalue.com/ontodb-v0.6.2-linux-x86_64.tar.gz
tar xzf ontodb-v0.6.2-linux-x86_64.tar.gz -C /opt/ontodb
export PATH=$PATH:/opt/ontodb/bin
```

### 2.3 Windows 瀹夎

```powershell
# PowerShell 涓€閿畨瑁?
irm https://get.ontovalue.com/install.ps1 | iex

# 鎴栨墜鍔ㄨВ鍘嬪埌鐩綍
```

### 2.4 Docker 閮ㄧ讲

```bash
# 鍗曡妭鐐?
docker run -d --name ontodb \
  -p 7912:7912 -p 7913:7913 \
  -v ontodb-data:/data \
  ontodb/ontodb:latest

# 浣跨敤 docker-compose
curl -O https://raw.githubusercontent.com/ontodb/ontodb/main/docker-compose.yml
docker-compose up -d
```

### 2.5 婧愮爜缂栬瘧

```bash
# 鍓嶇疆瑕佹眰锛歊ust 1.70+
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release

# 浜岃繘鍒舵枃浠朵綅浜?target/release/
```

### 2.6 鍚姩鍙傛暟

| 鍙傛暟 | 璇存槑 | 榛樿鍊?|
|------|------|--------|
| `--data-dir` | 鏁版嵁鐩綍 | `./data` |
| `--http` | HTTP 鐩戝惉鍦板潃 | `127.0.0.1:7912` |
| `--auth` | 鍚敤 API Key 璁よ瘉 | `false` |
| `--api-key` | API 瀵嗛挜 | 鑷姩鐢熸垚 |
| `--no-rate-limit` | 绂佺敤閫熺巼闄愬埗 | `false` |
| `--tls-cert` | TLS 璇佷功鏂囦欢 | 鏃?|
| `--tls-key` | TLS 绉侀挜鏂囦欢 | 鏃?|
| `--encryption-enabled` | 鍚敤瀛樺偍鍔犲瘑 | `false` |
| `--master-key-source` | 涓诲瘑閽ユ潵婧?| `env:ONTO_MASTER_KEY` |

---

## 3. SQL 璇硶

### 3.1 DDL锛堟暟鎹畾涔夛級

```sql
-- 鍒涘缓绫伙紙鎺ㄨ崘浣跨敤 OntoQL 璇硶锛?
CREATE CLASS users

-- 鍒涘缓甯︾户鎵跨殑绫?
CREATE CLASS Employee EXTENDS Person

-- 鍒涘缓瀹屾暣鏈綋锛堝涓被 + 灞炴€э級
CREATE ONTOLOGY MyApp (
    CLASS Product,
    CLASS Order,
    PROPERTY name DOMAIN Product RANGE STRING,
    PROPERTY price DOMAIN Product RANGE FLOAT64
)

-- 鍏煎鏃ц娉曪紙浠嶅彲鐢級
CREATE VERTEX TABLE users (
    id STRING,
    name STRING,
    age INT,
    email STRING
);
```

-- 鍒涘缓绱㈠紩
CREATE INDEX ON users (name);
CREATE INDEX ON users (age, name);

-- 鍒涘缓鍚戦噺绱㈠紩
CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine;

-- 鍒涘缓鐗╁寲瑙嗗浘
CREATE MATERIALIZED VIEW user_stats AS
SELECT age, COUNT(*) as cnt FROM users GROUP BY age;
```

### 3.2 DML锛堟暟鎹搷浣滐級

```sql
-- 鎻掑叆
INSERT INTO users (id, name, age, email) VALUES
    ('u1', 'Alice', 30, 'alice@example.com'),
    ('u2', 'Bob', 25, 'bob@example.com');

-- 鎵归噺鎻掑叆
BATCH INSERT INTO users (id, name, age) VALUES
    ('u3', 'Charlie', 35),
    ('u4', 'Diana', 28),
    ('u5', 'Eve', 42);

-- 鏇存柊
UPDATE users SET age = 31 WHERE name = 'Alice';

-- 鍒犻櫎
DELETE FROM users WHERE age < 20;

-- UPSERT (瀛樺湪鍒欐洿鏂帮紝涓嶅瓨鍦ㄥ垯鎻掑叆)
INSERT INTO users (id, name, age) VALUES ('u1', 'Alice Updated', 31)
ON CONFLICT (id) DO UPDATE SET name = excluded.name, age = excluded.age;
```

### 3.3 DQL锛堟暟鎹煡璇級

```sql
-- 鍩虹鏌ヨ
SELECT * FROM users WHERE age > 25 ORDER BY name LIMIT 10;

-- 鑱氬悎鏌ヨ
SELECT age, COUNT(*) as count, AVG(age) as avg_age
FROM users
GROUP BY age
HAVING COUNT(*) > 1;

-- JOIN 鏌ヨ
SELECT u.name, COUNT(k.to_id) as friend_count
FROM users u
LEFT JOIN knows k ON u.id = k.from_id
GROUP BY u.name;

-- 瀛愭煡璇?
SELECT * FROM users WHERE age > (SELECT AVG(age) FROM users);

-- CTE (鍏叡琛ㄨ〃杈惧紡)
WITH active_users AS (
    SELECT * FROM users WHERE age > 20
)
SELECT * FROM active_users WHERE name LIKE 'A%';

-- 绐楀彛鍑芥暟
SELECT name, age,
    ROW_NUMBER() OVER (ORDER BY age DESC) as rank,
    AVG(age) OVER () as avg_age
FROM users;

-- 鍥鹃亶鍘嗘煡璇?
GRAPH TRAVERSE FROM 'users::u1' OUT LABEL 'knows' DEPTH 3;

-- 鏈€鐭矾寰?
GRAPH SHORTEST PATH FROM 'users::u1' TO 'users::u5';
```

### 3.4 瀵煎叆瀵煎嚭

```sql
-- 浠?CSV 瀵煎叆
COPY users FROM '/path/to/users.csv' FORMAT CSV HEADER;

-- 瀵煎嚭鍒版枃浠?
COPY (SELECT * FROM users) TO '/path/to/export.csv' FORMAT CSV;

-- 浠?JSON 瀵煎叆
IMPORT users FROM '/path/to/users.json' FORMAT JSON;
```

### 3.5 OntoQL 璇硶锛堟湰浣撴煡璇㈣瑷€锛?

OntoQL 鏄?OntoDB 鐨勬湰浣撴煡璇㈣瑷€锛屽湪 SQL 鍩虹涓婂鍔犱簡**绫荤户鎵裤€佸睘鎬ц涔夈€佷笁鍏冪粍鎿嶄綔銆佹湰浣撴帹鐞?*鑳藉姏銆?

#### 3.5.1 鏈綋瀹氫箟

```sql
-- 鍒涘缓鍗曚釜绫伙紙鑷姩鍒涘缓鍚屽悕鏈綋锛?
CREATE CLASS Dog

-- 鍒涘缓甯︾户鎵跨殑绫?
CREATE CLASS Dog EXTENDS Animal

-- 鍒涘缓瀹屾暣鏈綋锛堝涓被 + 灞炴€?缁勭粐鍦ㄤ竴璧凤級
CREATE ONTOLOGY BioCompute (
    CLASS BioTask,
    CLASS ScreenResult,
    CLASS ScreenHit EXTENDS MeasurableEntity,
    PROPERTY task_name DOMAIN BioTask RANGE STRING,
    PROPERTY data_type DOMAIN BioTask RANGE STRING,
    PROPERTY status DOMAIN BioTask RANGE STRING,
    PROPERTY value_density DOMAIN BioTask RANGE FLOAT64
)

-- 鍒涘缓鍏变韩鍩虹被鏈綋
CREATE ONTOLOGY SharedBase (
    CLASS TimestampedEntity,
    CLASS OwnedEntity EXTENDS TimestampedEntity,
    CLASS MeasurableEntity EXTENDS OwnedEntity,
    PROPERTY created_at DOMAIN TimestampedEntity RANGE FLOAT64,
    PROPERTY owner DOMAIN OwnedEntity RANGE STRING,
    PROPERTY project_id DOMAIN OwnedEntity RANGE STRING,
    PROPERTY value_score DOMAIN MeasurableEntity RANGE FLOAT64
)
```

#### 3.5.2 鍒犻櫎

```sql
-- 鍒犻櫎鍗曚釜绫伙紙瀵瑰簲 CREATE CLASS锛?
DROP CLASS Dog

-- 鍒犻櫎鏁翠釜鏈綋锛堝搴?CREATE ONTOLOGY锛?
DROP ONTOLOGY BioCompute
```

**瀵瑰簲鍏崇郴**锛歚CREATE CLASS` 鈫?`DROP CLASS`锛宍CREATE ONTOLOGY` 鈫?`DROP ONTOLOGY`

#### 3.5.3 鏁版嵁鎿嶄綔锛圫ET 璇硶锛?

```sql
-- INSERT锛圤ntoQL SET 璇硶锛屾瘮 SQL VALUES 鏇寸畝娲侊級
INSERT INTO BioTask SET
    task_name = 'sample.fastq',
    data_type = 'fastq',
    status = 'completed',
    value_density = 0.75,
    owner = 'lab-01',
    created_at = 1724800000.0

-- SELECT锛堜笌 SQL 鐩稿悓锛?
SELECT * FROM BioTask WHERE status = 'completed'
SELECT * FROM BioTask WHERE owner = 'lab-01' ORDER BY value_density DESC

-- UPDATE / DELETE锛堜笌 SQL 鐩稿悓锛?
UPDATE BioTask SET status = 'archived' WHERE owner = 'lab-01'
DELETE FROM BioTask WHERE status = 'failed'
```

#### 3.5.4 缁ф壙鏌ヨ

```sql
-- 鏌ヨ Animal 浼氳嚜鍔ㄨ繑鍥?Dog銆丆at銆丅ird 绛夋墍鏈夊瓙绫诲疄渚?
SELECT * FROM Animal

-- 鏌ヨ Device 浼氳嚜鍔ㄨ繑鍥?Sensor銆乀empSensor銆丼martLight 绛?
SELECT * FROM Device

-- 鏌ョ湅瀹炰綋鐨勭湡瀹炵被鍨?
SELECT name, __class__ FROM Device
```

#### 3.5.5 涓夊厓缁勬搷浣滐紙RDF锛?

```sql
-- 鎻掑叆涓夊厓缁?
INSERT TRIPLE SET subject = "dog1", predicate = "rdf:type", object = "Dog"
INSERT TRIPLE SET subject = "dog1", predicate = "name", object = "Rex"

-- 鎵归噺鎻掑叆
INSERT TRIPLES (subject, predicate, object) VALUES
    ("dog1", "rdf:type", "Dog"),
    ("dog1", "name", "Rex"),
    ("dog1", "age", "3")

-- 鏌ヨ涓夊厓缁?
SELECT TRIPLE
SELECT TRIPLE WHERE subject = "dog1"
SELECT TRIPLE WHERE predicate = "rdf:type" LIMIT 10

-- 鍒犻櫎涓夊厓缁?
DELETE TRIPLE SET subject = "dog1", predicate = "name", object = "Rex"
```

#### 3.5.6 鎺ㄧ悊鏌ヨ

```sql
-- 浣跨敤 INFER 鍏抽敭瀛楀紑鍚?OWL 鎺ㄧ悊
SELECT * FROM Animal INFER @onto(scope=SUBCLASS)

-- 鎺ㄧ悊浼氳嚜鍔ㄥ睍寮€绫诲眰娆★細
-- Animal 鈫?Mammal 鈫?Dog, Cat
-- Animal 鈫?Bird 鈫?Eagle
-- 鏌ヨ Animal 杩斿洖鎵€鏈夊瓙绫诲疄渚?
```

#### 3.5.7 浜嬪姟

```sql
BEGIN
INSERT INTO BioTask SET task_name = 'txn-test', status = 'new'
UPDATE BioTask SET status = 'committed' WHERE task_name = 'txn-test'
COMMIT

-- 鎴栧洖婊?
BEGIN
DELETE FROM BioTask WHERE task_name = 'txn-test'
ROLLBACK
```

#### 3.5.8 OntoQL vs SQL vs SPARQL 瀵圭収

| 鎿嶄綔 | SQL | OntoQL | SPARQL |
|------|-----|--------|--------|
| 寤鸿〃 | `CREATE ONTOLOGY X (CLASS Y)` | `CREATE CLASS Y` | 涓嶆敮鎸?|
| 寤哄簱 | `CREATE ONTOLOGY X (...)` | `CREATE ONTOLOGY X (...)` | 涓嶆敮鎸?|
| 鍒犺〃 | 涓嶆敮鎸?| `DROP CLASS Y` | 涓嶆敮鎸?|
| 鍒犲簱 | 涓嶆敮鎸?| `DROP ONTOLOGY X` | 涓嶆敮鎸?|
| 鎻掓暟鎹?| `INSERT INTO T (...) VALUES (...)` | `INSERT INTO T SET col=val` | 涓嶆敮鎸?|
| 鏌ユ暟鎹?| `SELECT * FROM T` | `SELECT * FROM T` | `SELECT ?x WHERE {?x rdf:type T}` |
| 缁ф壙鏌ヨ | 涓嶆敮鎸?| `SELECT * FROM Animal`锛堣嚜鍔ㄥ睍寮€锛?| `?x rdf:type/rdfs:subClassOf* Animal` |
| 涓夊厓缁?| 涓嶆敮鎸?| `INSERT TRIPLE SET ...` | `INSERT DATA { ... }` |

> 搴旂敤灞傛棩甯?CRUD 鐢?SQL 鍗冲彲銆傜鐞嗘搷浣滐紙寤烘湰浣?鍔犲睘鎬?DROP锛夌敤 OntoQL銆係PARQL 閫傚悎鐭ヨ瘑鍥捐氨闆嗘垚銆?

---

## 4. 鍚戦噺鎼滅储

### 4.1 鍒涘缓鍚戦噺绱㈠紩

```sql
-- 鍒涘缓128缁村悜閲忕储寮曪紙浣欏鸡鐩镐技搴︼級
CREATE VECTOR INDEX ON documents (embedding)
    DIMENSIONS 128
    METRIC cosine;

-- 鍒涘缓256缁村悜閲忕储寮曪紙娆ф皬璺濈锛?
CREATE VECTOR INDEX ON images (feature_vector)
    DIMENSIONS 256
    METRIC euclidean;
```

### 4.2 鍚戦噺鎼滅储

```sql
-- 鍩虹鍚戦噺鎼滅储
VECTOR SEARCH ON documents (embedding)
    QUERY [0.1, 0.2, 0.3, ..., 0.128]
    TOP 10;

-- 甯﹁繃婊ゆ潯浠剁殑鍚戦噺鎼滅储
VECTOR SEARCH ON documents (embedding)
    QUERY [0.1, 0.2, 0.3, ..., 0.128]
    TOP 10
    WHERE category = '鎶€鏈?;
```

### 4.3 娣峰悎鏌ヨ锛圫QL + 鍚戦噺锛?

```sql
-- SQL 杩囨护 + 鍚戦噺鎺掑簭
SELECT title, VECTOR_DISTANCE(embedding, [0.1, 0.2, ...]) as score
FROM documents
WHERE category = '鎶€鏈? AND year > 2020
ORDER BY score
LIMIT 5;
```

### 4.4 HTTP API

```bash
# 鍚戦噺鎼滅储
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "documents",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3],
    "top_k": 10,
    "filter": "category = \"鎶€鏈痋""
  }'

# 娣峰悎鏌ヨ
curl -X POST http://127.0.0.1:7912/api/hybrid/query \
  -H "Content-Type: application/json" \
  -d '{
    "class": "documents",
    "vector_column": "embedding",
    "query_vector": [0.1, 0.2, 0.3],
    "filter": "year > 2020",
    "top_k": 5
  }'
```

---

## 5. 鍥炬煡璇?

### 5.1 鍒涘缓鍥剧粨鏋?

```sql
-- 鍒涘缓椤剁偣
INSERT VERTEX Person (id, name, age) VALUES ('p1', 'Alice', 30);
INSERT VERTEX Person (id, name, age) VALUES ('p2', 'Bob', 25);

-- 鍒涘缓杈?
INSERT EDGE knows (from_id, to_id, since) VALUES ('p1', 'p2', 2020);
```

### 5.2 鍥鹃亶鍘?

```sql
-- BFS 閬嶅巻锛堜粠 p1 鍑哄彂锛? 璺筹級
GRAPH TRAVERSE FROM 'Person::p1' OUT LABEL 'knows' DEPTH 3;

-- DFS 閬嶅巻
GRAPH TRAVERSE FROM 'Person::p1' OUT DEPTH 5 ALGORITHM dfs;

-- 甯﹁繃婊ょ殑閬嶅巻
GRAPH TRAVERSE FROM 'Person::p1' OUT DEPTH 2 WHERE age > 25;

-- 鏈€鐭矾寰?
GRAPH SHORTEST PATH FROM 'Person::p1' TO 'Person::p5';
```

### 5.3 HTTP API

```bash
# 鍥鹃亶鍘?
curl -X POST http://127.0.0.1:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -d '{
    "start": "Person::p1",
    "direction": "out",
    "edge_label": "knows",
    "max_depth": 3,
    "algorithm": "bfs"
  }'

# 鏈€鐭矾寰?
curl -X POST http://127.0.0.1:7912/api/graph/shortest-path \
  -H "Content-Type: application/json" \
  -d '{"from": "Person::p1", "to": "Person::p5"}'
```

---

## 6. SPARQL 鏌ヨ

### 6.1 鍩虹鏌ヨ

```sparql
-- 鏌ヨ鎵€鏈?Person
SELECT ?name ?age
WHERE {
    ?person rdf:type :Person .
    ?person :name ?name .
    ?person :age ?age .
}
ORDER BY ?name
LIMIT 10;
```

### 6.2 杩囨护鏌ヨ

```sparql
-- 杩囨护骞撮緞澶т簬 25 鐨?Person
SELECT ?name ?age
WHERE {
    ?person rdf:type :Person .
    ?person :name ?name .
    ?person :age ?age .
    FILTER(?age > 25)
}
ORDER BY ?age DESC;
```

### 6.3 OPTIONAL 鏌ヨ

```sparql
-- 鏌ヨ Person 鍙婂叾鍙€夌殑閭
SELECT ?name ?email
WHERE {
    ?person rdf:type :Person .
    ?person :name ?name .
    OPTIONAL { ?person :email ?email }
};
```

### 6.4 CONSTRUCT 鏌ヨ

```sparql
-- 鏋勯€犳柊鐨?RDF 鍥?
CONSTRUCT {
    ?person :hasFriend ?friend .
}
WHERE {
    ?person :knows ?friend .
    ?friend :age ?age .
    FILTER(?age > 20)
};
```

### 6.5 ASK 鏌ヨ

```sparql
-- 妫€鏌ユ槸鍚﹀瓨鍦?
ASK {
    ?person rdf:type :Person .
    ?person :name "Alice" .
};
```

---

## 7. 鏈綋鎺ㄧ悊

### 7.1 鍒涘缓鏈綋

```sql
CREATE ONTOLOGY MyOntology (
    CLASS Animal,
    CLASS Dog SUBCLASS OF Animal,
    CLASS Cat SUBCLASS OF Animal,
    CLASS Pet SUBCLASS OF Animal,
    CLASS GuardDog SUBCLASS OF Dog,
    
    PROPERTY hasName DOMAIN Animal RANGE STRING,
    PROPERTY hasAge DOMAIN Animal RANGE INT,
    PROPERTY belongsTo DOMAIN Pet RANGE Person,
    
    CLASS Person,
    PROPERTY owns DOMAIN Person RANGE Pet
);
```

### 7.2 鑷姩鎺ㄧ悊

```sql
-- 鎻掑叆瀹炰緥
INSERT INTO Dog (hasName, hasAge) VALUES ('鏃鸿储', 3);

-- 鏌ヨ鎵€鏈?Animal锛圖og 鑷姩鍖呭惈鍦ㄥ唴锛?
SELECT * FROM Animal;
-- 缁撴灉鍖呭惈锛氭椇璐紙Dog 鏄?Animal 鐨勫瓙绫伙級

-- 鏌ヨ鎵€鏈?Pet锛圖og 涔熸槸 Pet锛?
SELECT * FROM Pet;
```

### 7.3 鎺ㄧ悊瑙勫垯

OntoDB 鏀寔 7 鏉?OWL 2 RL 鎺ㄧ悊瑙勫垯锛?

| 瑙勫垯 | 璇存槑 | 绀轰緥 |
|------|------|------|
| CaxSco | 绫荤户鎵挎帹鐞?| Dog 鈯?Animal 鈫?鏃鸿储 鈭?Animal |
| CaxEqc | 绛変环绫绘帹鐞?| Dog 鈮?Canine 鈫?鏃鸿储 鈭?Canine |
| PrpSpo | 灞炴€х户鎵挎帹鐞?| hasOwner 鈯?hasBelonging 鈫?浼犻€?|
| PrpEqp | 绛変环灞炴€ф帹鐞?| hasName 鈮?getName 鈫?璇箟鍒悕 |
| PrpInv | 鍙嶅悜灞炴€ф帹鐞?| owns 鈫?ownedBy |
| PrpTrp | 浼犻€掑睘鎬ф帹鐞?| ancestorOf 浼犻€?|
| PrpSymp | 瀵圭О灞炴€ф帹鐞?| friendOf 瀵圭О |

### 7.4 鎺ㄧ悊瑙ｉ噴

```sql
-- 鏌ョ湅鎺ㄥ閾?
EXPLAIN SELECT * FROM Animal WHERE hasName = '鏃鸿储';

-- 杈撳嚭锛?
-- 鏃鸿储 鈭?Dog (鐩存帴鏂█)
-- Dog 鈯?Animal (鏈綋瑙勫垯)
-- 鈫?鏃鸿储 鈭?Animal (鎺ㄥ)
```

---

## 8. 浜嬪姟绠＄悊

### 8.1 鍩虹浜嬪姟

```sql
-- 寮€濮嬩簨鍔?
BEGIN;

-- 鎵ц鎿嶄綔
INSERT INTO users (name, age) VALUES ('Alice', 30);
UPDATE accounts SET balance = balance - 100 WHERE user = 'Alice';

-- 鎻愪氦浜嬪姟
COMMIT;

-- 鎴栧洖婊?
ROLLBACK;
```

### 8.2 闅旂绾у埆

OntoDB 浣跨敤 **蹇収闅旂**锛圫napshot Isolation锛夛細
- 姣忎釜浜嬪姟鐪嬪埌涓€鑷寸殑鏁版嵁蹇収
- 鍐欏叆鍐茬獊鏃惰嚜鍔ㄥ洖婊?
- 閫傚悎璇诲鍐欏皯鐨勫満鏅?

---

## 9. 澶囦唤鎭㈠

### 9.1 鍏ㄩ噺澶囦唤

```sql
-- SQL 鏂瑰紡
BACKUP TO '/backups/full-2026-08-10.ontodb';

-- HTTP API
curl -X POST http://127.0.0.1:7912/api/backup \
  -H "Content-Type: application/json" \
  -d '{"path": "/backups/full-2026-08-10.ontodb"}'
```

### 9.2 澧為噺澶囦唤

```sql
-- 澧為噺澶囦唤锛堜粎澶囦唤鍙樻洿閮ㄥ垎锛?
BACKUP INCREMENTAL TO '/backups/incr-2026-08-10.ontodb';
```

### 9.3 鎭㈠

```sql
-- SQL 鏂瑰紡
RESTORE FROM '/backups/full-2026-08-10.ontodb';

-- HTTP API
curl -X POST http://127.0.0.1:7912/api/restore \
  -H "Content-Type: application/json" \
  -d '{"path": "/backups/full-2026-08-10.ontodb"}'
```

### 9.4 鑷姩澶囦唤鑴氭湰

```bash
#!/bin/bash
# 姣忔棩鍑屾櫒 2 鐐硅嚜鍔ㄥ浠?
BACKUP_DIR="/backups/ontodb"
DATE=$(date +%Y%m%d)
curl -X POST http://127.0.0.1:7912/api/backup \
  -H "Content-Type: application/json" \
  -d "{\"path\": \"$BACKUP_DIR/backup-$DATE.ontodb\"}"
```

---

## 10. 瀹夊叏閰嶇疆

### 10.1 API Key 璁よ瘉

```bash
# 鍚姩鏃跺惎鐢ㄨ璇?
./ontodb-server --auth --api-key your-secret-key

# 浣跨敤 API Key 璁块棶
curl -H "Authorization: Bearer your-secret-key" \
  http://127.0.0.1:7912/api/query \
  -d '{"query": "SELECT * FROM users"}'
```

### 10.2 TLS/HTTPS

```bash
# 浣跨敤璇佷功鍚姩
./ontodb-server --tls-cert cert.pem --tls-key key.pem

# 鑷鍚嶈瘉涔︼紙寮€鍙戠幆澧冿級
./ontodb-server --tls-cert self-signed.crt --tls-key self-signed.key
```

### 10.3 IP 鐧藉悕鍗?

```json
// config/api_keys.json
{
  "keys": [
    {
      "key": "your-api-key",
      "name": "Admin",
      "permissions": ["read", "write", "admin"],
      "ip_whitelist": ["192.168.1.0/24", "10.0.0.1"]
    }
  ]
}
```

### 10.4 瀛樺偍鍔犲瘑

```bash
# 浣跨敤鐜鍙橀噺瀛樺偍涓诲瘑閽?
export ONTO_MASTER_KEY=$(openssl rand -hex 32)
./ontodb-server --encryption-enabled --master-key-source env:ONTO_MASTER_KEY

# 浣跨敤 KMS
./ontodb-server --encryption-enabled \
  --master-key-source "kms|https://vault.example.com/v1/transit|ontodb-master|hvs.xxx"
```

### 10.5 RBAC锛堜紒涓氱増锛?

```sql
-- 鍒涘缓瑙掕壊
CREATE ROLE SystemAdmin;
CREATE ROLE SecurityAdmin;
CREATE ROLE AuditAdmin;

-- 鍒嗛厤鏉冮檺
GRANT ALL ON * TO SystemAdmin;
GRANT READ ON * TO SecurityAdmin;
GRANT SELECT ON audit_logs TO AuditAdmin;

-- 鍒嗛厤鐢ㄦ埛瑙掕壊
GRANT SystemAdmin TO user 'admin';
```

---

## 11. 鎬ц兘璋冧紭

### 11.1 閰嶇疆鍙傛暟

```toml
# ontodb.toml

[data]
dir = "/data/ontodb"

[performance]
# MemTable 澶у皬锛堝澶у彲鍑忓皯 flush 棰戠巼锛?
memtable_size_mb = 128

# Block Cache 澶у皬锛堝澶у彲鎻愬崌璇诲彇鎬ц兘锛?
block_cache_mb = 512

# WAL fsync 绛栫暐
sync_wal_on_commit = false  # 璁句负 true 鍙繚璇佹寔涔呮€э紝浣嗛檷浣庢€ц兘

# 鍚庡彴鍘嬬缉绾跨▼鏁?
compaction_threads = 4

[server]
# 杩炴帴姹犲ぇ灏?
max_connections = 1000

# 璇锋眰瓒呮椂
request_timeout_secs = 30
```

### 11.2 鍩哄噯娴嬭瘯

```bash
# 杩愯鍩哄噯娴嬭瘯
cargo bench --bench lock_contention -p onto-storage
cargo bench --bench batch_import -p onto-storage

# 棰勬湡缁撴灉锛?
# 鍐欏叆: 767,561 ops/s
# 璇诲彇: 1,176,147 ops/s
# 鎵归噺鍐欏叆: 967,453 ops/s
```

### 11.3 鐩戞帶鎸囨爣

```bash
# Prometheus 鎸囨爣
curl http://127.0.0.1:7912/metrics

# JSON 鎸囨爣
curl http://127.0.0.1:7912/api/metrics

# 鍏抽敭鎸囨爣锛?
# - ontodb_queries_total: 鎬绘煡璇㈡暟
# - ontodb_query_latency: 鏌ヨ寤惰繜鍒嗗竷
# - ontodb_cache_hits: 缂撳瓨鍛戒腑鏁?
# - ontodb_memtable_size_bytes: MemTable 澶у皬
# - ontodb_disk_usage_bytes: 纾佺洏浣跨敤閲?
```

---

## 12. 鏁呴殰鎺掓煡

### 12.1 甯歌闂

| 闂 | 鍘熷洜 | 瑙ｅ喅鏂规 |
|------|------|---------|
| 杩炴帴琚嫆缁?| 鏈嶅姟鍣ㄦ湭鍚姩鎴栫鍙ｉ敊璇?| 妫€鏌?`ps aux | grep ontodb` 鍜岀鍙?|
| 鏌ヨ瓒呮椂 | 鏌ヨ澶鏉傛垨鏁版嵁閲忓お澶?| 娣诲姞 LIMIT锛屼紭鍖?WHERE 鏉′欢 |
| 鍐呭瓨涓嶈冻 | MemTable 鎴?Cache 澶ぇ | 鍑忓皬 `memtable_size_mb` 鍜?`block_cache_mb` |
| 纾佺洏婊?| WAL 鎴?SSTable 绱Н | 娓呯悊鏃ф暟鎹垨鎵╁纾佺洏 |
| 璁よ瘉澶辫触 | API Key 閿欒 | 妫€鏌?`--api-key` 閰嶇疆 |

### 12.2 鏃ュ織鏌ョ湅

```bash
# 鍚敤璋冭瘯鏃ュ織
RUST_LOG=debug ./ontodb-server --data-dir ./data

# 鏌ョ湅鐗瑰畾妯″潡鏃ュ織
RUST_LOG=onto_storage=debug,onto_query=info ./ontodb-server
```

### 12.3 鍋ュ悍妫€鏌?

```bash
# 鍋ュ悍妫€鏌?
curl http://127.0.0.1:7912/api/health
# {"status": "ok", "version": "0.3.0"}

# 灏辩华妫€鏌ワ紙K8s锛?
curl http://127.0.0.1:7912/api/health/ready

# 瀛樻椿妫€鏌ワ紙K8s锛?
curl http://127.0.0.1:7912/api/health/live
```

---

## 闄勫綍

### A. 閿欒鐮?

| 閿欒鐮?| 璇存槑 | 澶勭悊寤鸿 |
|--------|------|---------|
| 400 | 璇锋眰鏍煎紡閿欒 | 妫€鏌?JSON 鏍煎紡 |
| 401 | 璁よ瘉澶辫触 | 妫€鏌?API Key |
| 429 | 閫熺巼闄愬埗 | 绛夊緟鎴栫鐢ㄩ檺娴?|
| 500 | 鏈嶅姟鍣ㄥ唴閮ㄩ敊璇?| 鏌ョ湅鏃ュ織 |

### B. 绔彛璇存槑

| 绔彛 | 鍗忚 | 璇存槑 |
|------|------|------|
| 7912 | HTTP | REST API |
| 7913 | TCP | PostgreSQL Wire Protocol |
| 7914 | TCP | MySQL Wire Protocol |

### C. 鏁版嵁绫诲瀷

| 绫诲瀷 | 璇存槑 | 绀轰緥 |
|------|------|------|
| STRING | 瀛楃涓?| `'hello'` |
| INT | 64浣嶆暣鏁?| `42` |
| DOUBLE | 64浣嶆诞鐐规暟 | `3.14` |
| BOOL | 甯冨皵鍊?| `TRUE` / `FALSE` |
| ARRAY | 鏁扮粍 | `[1, 2, 3]` |
| JSON | JSON 瀵硅薄 | `{"key": "value"}` |
| BLOB | 浜岃繘鍒舵暟鎹?| `'\x010203'` |
