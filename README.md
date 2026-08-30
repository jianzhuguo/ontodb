# OntoDB 鈥?鏈綋椹卞姩鐨勫叚妯℃€佽涔夋暟鎹簱

**[English](README.en.md)** | 涓枃

<p align="center">
  <b>鍏ㄧ悆棣栦釜灏?OWL 鎺ㄧ悊寮曟搸宓屽叆鏁版嵁搴撳唴鏍哥殑鍏ā鎬佺粺涓€璇箟鏁版嵁搴?/b>
</p>

---

## 鏍稿績鐗规€?
| 鐗规€?| 璇存槑 |
|------|------|
| **鍏ā鎬佺粺涓€瀛樺偍** | 鍏崇郴鍨?+ 鍥?+ 鍚戦噺 + 鏃跺簭 + 绌洪棿 + 鏈綋锛屼竴濂?API 缁熶竴鏌ヨ |
| **鏁版嵁鎻掑叆鍗崇敓鎴愯涔?* | INSERT 鏃惰嚜鍔ㄦ墽琛?7 姝ヨ仈鍔細鏂囨。鍐欏叆 鈫?鍥鹃《鐐?鈫?rdf:type 涓夊厓缁?鈫?灞炴€т笁鍏冪粍 鈫?OWL 鎺ㄧ悊 鈫?鍚戦噺绱㈠紩 鈫?B+Tree 绱㈠紩 |
| **瀹炴椂鏈綋鎺ㄧ悊** | 7 鏉?OWL 2 RL 瑙勫垯锛屽閲忎笉鍔ㄧ偣绠楁硶锛屾帹瀵奸摼鍙拷婧?|
| **璇箟鍚戦噺娣峰悎鏌ヨ** | HNSW 鍚戦噺绱㈠紩 + SQL/SPARQL 鑱斿悎鏌ヨ |
| **鑷€傚簲鍐呭瓨绠＄悊** | MemTable 4-256MB 鍔ㄦ€佽皟鏁达紝Block Cache 16MB-1GB 鑷€傚簲锛岀郴缁熷帇鍔涜嚜鍔ㄦ敹缂?|
| **鍏ㄥ煙缁勬彁浜?* | put/commit_txn/put_batch 鍏ㄨ矾寰勭粍鎻愪氦锛屾贩鍚堟壒閲忕瓥鐣?|
| **鍐呭瓨瀹夊叏** | 绾?Rust 瀹炵幇锛岄浂 unsafe 浠ｇ爜鍧楋紝430 澶?expect |
| **PostgreSQL/MySQL 鍏煎** | 鏀寔 PG Wire 鍜?MySQL 鍗忚锛岀幇鏈夊鎴风鐩存帴杩炴帴 |

## 鎬ц兘鍩哄噯

| 鎸囨爣 | 鏁板€?|
|------|------|
| 鍐欏叆鍚炲悙閲?| 863,618 ops/s |
| 璇诲彇鍚炲悙閲?| 1,256,518 ops/s |
| 鎵归噺鍐欏叆 | 1,082,230 ops/s |
| HNSW 鍚戦噺鍙洖鐜?| 100%锛坋f_search=200锛屽欢杩?341碌s锛?|
| 8绾跨▼骞跺彂鍔犻€?| 1.82x |
| GIS 绌洪棿鍏崇郴鍒ゆ柇 | 鈮?碌s |
| TSM 鍘嬬缉鐜?| 60%+ |

## 蹇€熷紑濮?
### 鏂瑰紡涓€锛氫粠婧愮爜缂栬瘧

```bash
# 鍓嶇疆瑕佹眰锛歊ust 1.70+
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release

# 鍚姩鏈嶅姟鍣?./target/release/ontodb-server --data-dir ./data --http 0.0.0.0:7912
```

### 鏂瑰紡浜岋細Docker

```bash
docker run -p 7912:7912 ontodb/ontodb-server --data-dir /data --http 0.0.0.0:7912
```

## 鍚姩鏈嶅姟鍣?
```bash
# 鍩烘湰鍚姩
./ontodb-server --data-dir ./data --http 127.0.0.1:7912

# 鍚敤璁よ瘉
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --auth --api-key your-secret-key

# 绂佺敤閫熺巼闄愬埗锛堝紑鍙?娴嬭瘯锛?./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --no-rate-limit
```

### 鍛戒护琛屽弬鏁?
| 鍙傛暟 | 璇存槑 | 榛樿鍊?|
|------|------|--------|
| `--data-dir` | 鏁版嵁鐩綍 | `./data` |
| `--http` | HTTP 鐩戝惉鍦板潃 | `127.0.0.1:7912` |
| `--auth` | 鍚敤 API Key 璁よ瘉 | `false` |
| `--api-key` | API 瀵嗛挜 | 鑷姩鐢熸垚 |
| `--no-rate-limit` | 绂佺敤閫熺巼闄愬埗 | `false` |

## 璁块棶鏂瑰紡

### HTTP REST API

```bash
# 鍋ュ悍妫€鏌?curl http://127.0.0.1:7912/api/health

# 鎵ц OntoQL
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users LIMIT 10"}'

# 鍚戦噺鎼滅储
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{"class": "documents", "column": "embedding", "query_vector": [0.1, 0.2], "top_k": 5}'
```

### Web 鎺у埗鍙?
鎵撳紑娴忚鍣ㄨ闂細`http://127.0.0.1:7912`

### SDK

| 璇█ | 鐩綍 |
|------|------|
| Python | `sdk/python/` |
| JavaScript/TypeScript | `sdk/javascript/` / `sdk/typescript/` |
| Go | `sdk/go/` |
| Java | `sdk/java/` |

## SQL 璇硶绀轰緥

```sql
-- 鍒涘缓琛?CREATE VERTEX TABLE users (name STRING, age INT, email STRING);

-- 鎻掑叆鏁版嵁
INSERT INTO users (name, age, email) VALUES ('寮犱笁', 30, 'zhangsan@example.com');

-- 鏌ヨ
SELECT * FROM users WHERE age > 25 ORDER BY name LIMIT 10;

-- 鏇存柊
UPDATE users SET age = 31 WHERE name = '寮犱笁';

-- 鍒犻櫎
DELETE FROM users WHERE name = '寮犱笁';
```

### 鍚戦噺鎼滅储

```sql
-- 鍒涘缓鍚戦噺绱㈠紩
CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine;

-- 鍚戦噺鎼滅储
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2, 0.3] TOP 10;
```

### 鍥炬煡璇?
```sql
-- 鍥鹃亶鍘?GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;

-- 鏈€鐭矾寰?GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5';
```

## 椤圭洰缁撴瀯

```
ontodb/
鈹溾攢鈹€ crates/
鈹?  鈹溾攢鈹€ onto-core/          # 鏍稿績绫诲瀷銆乀rait
鈹?  鈹溾攢鈹€ onto-storage/       # LSM-Tree 瀛樺偍寮曟搸
鈹?  鈹溾攢鈹€ onto-query/         # 鏌ヨ寮曟搸锛圫QL/OntoQL锛?鈹?  鈹溾攢鈹€ onto-graph/         # 鍥炬暟鎹ā鍨?鈹?  鈹溾攢鈹€ onto-ontology/      # 鏈綋鎺ㄧ悊寮曟搸
鈹?  鈹溾攢鈹€ onto-server/        # HTTP 鏈嶅姟鍣?鈹?  鈹溾攢鈹€ onto-cli/           # 鍛戒护琛屽伐鍏?鈹?  鈹溾攢鈹€ onto-enterprise/    # 浼佷笟鐗堝姛鑳?鈹?  鈹溾攢鈹€ onto-raft/          # Raft 鍒嗗竷寮?鈹?  鈹溾攢鈹€ onto-sharding/      # 鏁版嵁鍒嗙墖
鈹?  鈹溾攢鈹€ onto-plugin/        # 鎻掍欢妗嗘灦
鈹?  鈹溾攢鈹€ onto-edge/          # 杈圭紭璁惧
鈹?  鈹斺攢鈹€ onto-edge-esp32/    # ESP32 鏀寔
鈹溾攢鈹€ sdk/                    # 澶氳瑷€ SDK
鈹溾攢鈹€ examples/               # 绀轰緥搴旂敤
鈹溾攢鈹€ docs/                   # 鏂囨。
鈹斺攢鈹€ frontend/               # Web 鎺у埗鍙?```

## 璐＄尞

娆㈣繋璐＄尞锛佽鏌ョ湅 [CONTRIBUTING.md](CONTRIBUTING.md)銆?
## 璁稿彲璇?
OntoDB 閲囩敤鍙岃鍙瘉妯″紡锛?
| 鐗堟湰 | 璁稿彲璇?| 璇存槑 |
|------|--------|------|
| **绀惧尯鐗?* | AGPL-3.0 | 鍏嶈垂浣跨敤锛屼慨鏀瑰悗闇€寮€婧?|
| **浼佷笟鐗?* | 鍟嗕笟璁稿彲 | 鐢熶骇鐜浣跨敤锛岄渶璐拱璁稿彲璇?|

**绀惧尯鐗堬紙AGPL-3.0锛?*锛?- 鍙互鍏嶈垂浣跨敤銆佷慨鏀广€佸垎鍙?- 濡傛灉鎻愪緵缃戠粶鏈嶅姟锛屼慨鏀瑰悗鐨勪唬鐮佸繀椤诲紑婧?- 璇﹁ [LICENSE](LICENSE)

**浼佷笟鐗堬紙鍟嗕笟璁稿彲锛?*锛?- 鐢熶骇鐜浣跨敤
- 涓嶉渶瑕佸紑婧愪慨鏀瑰悗鐨勪唬鐮?- 鍖呭惈鎶€鏈敮鎸佸拰 SLA
- 璇﹁ [LICENSE.COMMERCIAL](LICENSE.COMMERCIAL)
- 鑱旂郴鏂瑰紡锛歭icense@ontovalue.com
