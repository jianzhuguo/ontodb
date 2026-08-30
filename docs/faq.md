# OntoDB 甯歌闂 (FAQ)

---

## 瀹夎閮ㄧ讲

### Q: 濡備綍瀹夎 OntoDB锛?
```bash
# Linux 涓€閿畨瑁?curl -fsSL https://get.ontovalue.com/install.sh | bash

# Docker
docker run -d -p 7912:7912 -v ontodb-data:/data ontodb/ontodb:latest
```

### Q: 鏀寔鍝簺鎿嶄綔绯荤粺锛?
- Linux x86_64 (Ubuntu 20.04+, CentOS 8+)
- Windows 10/11 x86_64
- macOS (Intel/Apple Silicon)
- Docker (鎵€鏈夊钩鍙?

### Q: 鏈€浣庣‖浠惰姹傦紵

- CPU: 2 鏍?- 鍐呭瓨: 2 GB
- 纾佺洏: 10 GB SSD

### Q: 濡備綍鍚姩鏈嶅姟鍣紵

```bash
./ontodb-server --data-dir ./data --http 127.0.0.1:7912
```

---

## 杩炴帴璁块棶

### Q: 鏈夊摢浜涜闂柟寮忥紵

| 鏂瑰紡 | 绔彛 | 璇存槑 |
|------|------|------|
| HTTP REST API | 7912 | 涓昏鎺ュ彛 |
| PostgreSQL Wire | 7913 | psql/pgAdmin |
| MySQL Wire | 7914 | mysql/Workbench |
| Web 鎺у埗鍙?| 7912/console | 娴忚鍣?|
| CLI | 鈥?| 鍛戒护琛屽伐鍏?|

### Q: 濡備綍浣跨敤 psql 杩炴帴锛?
```bash
psql -h 127.0.0.1 -p 7913 -U ontodb
```

### Q: 濡備綍鍚敤璁よ瘉锛?
```bash
./ontodb-server --auth --api-key "your-secret-key"

# 浣跨敤鏃跺甫涓?API Key
curl -H "Authorization: Bearer your-secret-key" \
  http://127.0.0.1:7912/api/query \
  -d '{"query": "SELECT * FROM users"}'
```

---

## SQL 鏌ヨ

### Q: 鏀寔鍝簺 SQL 璇彞锛?
- DDL: CREATE TABLE, CREATE INDEX, DROP TABLE
- DML: INSERT, UPDATE, DELETE, BATCH INSERT
- DQL: SELECT, WHERE, GROUP BY, HAVING, ORDER BY, LIMIT
- 楂樼骇: JOIN, CTE, 绐楀彛鍑芥暟, 瀛愭煡璇?
### Q: 濡備綍鍒涘缓鍚戦噺绱㈠紩锛?
```sql
CREATE VECTOR INDEX ON documents (embedding)
    DIMENSIONS 128
    METRIC cosine;
```

### Q: 濡備綍杩涜鍚戦噺鎼滅储锛?
```sql
VECTOR SEARCH ON documents (embedding)
    QUERY [0.1, 0.2, 0.3, ...]
    TOP 10;
```

### Q: 濡備綍杩涜鍥鹃亶鍘嗭紵

```sql
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;
```

### Q: 鏌ヨ瓒呮椂鎬庝箞鍔烇紵

```sql
-- 娣诲姞 LIMIT
SELECT * FROM large_table LIMIT 1000;

-- 浼樺寲 WHERE 鏉′欢
SELECT * FROM users WHERE id = 'u1';  -- 浣跨敤绱㈠紩瀛楁
```

---

## 鎬ц兘

### Q: 鍐欏叆鎬ц兘鏄灏戯紵

鍗曟満绾?80 涓?ops/s锛圢VMe SSD锛夈€?
### Q: 璇诲彇鎬ц兘鏄灏戯紵

鍗曟満绾?120 涓?ops/s銆?
### Q: 濡備綍鎻愬崌鎬ц兘锛?
1. 澧炲姞 `memtable_size_mb`锛堝噺灏?flush锛?2. 澧炲姞 `block_cache_mb`锛堟彁鍗囪鍙栵級
3. 浣跨敤 NVMe SSD
4. 鎵归噺鍐欏叆浠ｆ浛閫愭潯鍐欏叆

### Q: 鍐呭瓨鍗犵敤澶氬皯锛?
榛樿绾?400 MB锛?4 MB MemTable + 256 MB Cache + 鍏朵粬锛夈€?
---

## 鍚戦噺鎼滅储

### Q: 鏀寔鍝簺璺濈搴﹂噺锛?
- `cosine` 鈥?浣欏鸡鐩镐技搴?- `euclidean` 鈥?娆ф皬璺濈
- `dot` 鈥?鐐圭Н

### Q: 鏈€澶ф敮鎸佸灏戠淮锛?
4096 缁淬€?
### Q: 鏋勫缓 HNSW 绱㈠紩闇€瑕佸涔咃紵

5000 脳 128D 鍚戦噺绾?2.7 绉掋€?
### Q: 濡備綍娣峰悎 SQL 鍜屽悜閲忔悳绱紵

```sql
SELECT title, VECTOR_DISTANCE(embedding, [0.1, 0.2, ...]) as score
FROM documents
WHERE category = '鎶€鏈?
ORDER BY score
LIMIT 5;
```

---

## 鍥炬煡璇?
### Q: 鏈€澶ч亶鍘嗘繁搴︽槸澶氬皯锛?
榛樿鏈€澶?100 璺炽€?
### Q: 濡備綍鎵炬渶鐭矾寰勶紵

```sql
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5';
```

### Q: 濡備綍杩囨护杈圭被鍨嬶紵

```sql
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 2;
```

---

## 鏈綋鎺ㄧ悊

### Q: 鏀寔鍝簺鎺ㄧ悊瑙勫垯锛?
- CaxSco 鈥?绫荤户鎵?- CaxEqc 鈥?绛変环绫?- PrpSpo 鈥?灞炴€х户鎵?- PrpEqp 鈥?绛変环灞炴€?- PrpInv 鈥?鍙嶅悜灞炴€?- PrpTrp 鈥?浼犻€掑睘鎬?- PrpSymp 鈥?瀵圭О灞炴€?
### Q: 濡備綍鏌ョ湅鎺ㄥ閾撅紵

```sql
EXPLAIN SELECT * FROM Animal WHERE hasName = '鏃鸿储';
```

---

## 澶囦唤鎭㈠

### Q: 濡備綍澶囦唤锛?
```bash
curl -X POST http://localhost:7912/api/backup \
  -d '{"path": "/backups/backup.ontodb"}'
```

### Q: 濡備綍鎭㈠锛?
```bash
curl -X POST http://localhost:7912/api/restore \
  -d '{"path": "/backups/backup.ontodb"}'
```

### Q: 鏀寔澧為噺澶囦唤鍚楋紵

鏀寔銆?
```bash
curl -X POST http://localhost:7912/api/backup/incremental \
  -d '{"path": "/backups/incr.ontodb"}'
```

---

## 闆嗙兢

### Q: 濡備綍閮ㄧ讲闆嗙兢锛?
```bash
# 鑺傜偣 1
./ontodb-server --raft --raft-id 1 --raft-peers "2@node2:7913"

# 鑺傜偣 2
./ontodb-server --raft --raft-id 2 --raft-peers "1@node1:7913"
```

### Q: 鏈€灏忛泦缇よ妯★紵

3 鑺傜偣锛堟弧瓒?Quorum锛夈€?
### Q: 濡備綍瀹炵幇璐熻浇鍧囪　锛?
浣跨敤 Nginx 鍙嶅悜浠ｇ悊銆?
---

## SDK

### Q: 鏀寔鍝簺璇█锛?
- Python: `pip install ontodb`
- JavaScript/TypeScript: `npm install ontodb`
- Go: `go get github.com/ontodb/ontodb-go`
- Java: Maven `io.ontodb:ontodb-java`

### Q: Python SDK 绀轰緥锛?
```python
from ontodb import OntoDB

db = OntoDB("http://localhost:7912", api_key="your-key")
rows = db.query("SELECT * FROM users")
```

---

## 鏁呴殰鎺掓煡

### Q: 杩炴帴琚嫆缁濓紵

1. 妫€鏌ユ湇鍔″櫒鏄惁鍚姩: `ps aux | grep ontodb`
2. 妫€鏌ョ鍙? `netstat -tlnp | grep 7912`
3. 妫€鏌ラ槻鐏: `ufw status`

### Q: 鏌ヨ瓒呮椂锛?
1. 娣诲姞 LIMIT
2. 浼樺寲 WHERE 鏉′欢
3. 鍒涘缓绱㈠紩

### Q: 鍐呭瓨涓嶈冻锛?
鍑忓皬閰嶇疆:
```toml
memtable_size_mb = 32
block_cache_mb = 128
```

### Q: 纾佺洏婊★紵

1. 娓呯悊鏃ф暟鎹?2. 鎵╁纾佺洏
3. 鍚敤鍘嬬缉

### Q: 濡備綍鏌ョ湅鏃ュ織锛?
```bash
# systemd
journalctl -u ontodb -f

# Docker
docker logs -f ontodb
```
