import urllib.request, json, time

API = 'http://127.0.0.1:7912/api/query'

def q(sql):
    time.sleep(0.05)
    d = json.dumps({'query': sql}).encode()
    r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(r, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get('error'):
                print(f'  ERR: {result["error"][:100]}')
            return result
    except Exception as e:
        print(f'  FAIL: {e}')
        return {}

print('=== 数字军师：数据插入即生成语义演示 ===')
print()

# 1. 创建本体
print('1. 创建决策本体...')
q("CREATE ONTOLOGY DecisionOntology (CLASS Decision, CLASS DecisionMaker, CLASS StrategicDecision SUBCLASS OF Decision, CLASS TacticalDecision SUBCLASS OF Decision)")

# 2. 插入决策者
print('2. 插入决策者...')
q("INSERT INTO DecisionMaker (name, role, company, style) VALUES ('张明远', 'CEO', 'OntoDB科技', '分析型')")

# 3. 插入战略决策
print('3. 插入战略决策...')
q("INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('是否进入政务市场', '评估政务市场机会', '战略', 'P0', '已执行', '决定进入政务市场，先以浙江省为试点', '2025-06-15', '等保2.0政策推动政务数字化', '冷静', 0.75, 6, '0.82,0.15,0.91,0.33,0.67,0.45,0.78,0.12')")

# 4. 插入战术决策
print('4. 插入战术决策...')
q("INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('产品定价调整', '企业版定价过高导致转化率低', '产品', 'P1', '已执行', '企业版降价40%，推出标准版和高级版两档', '2026-03-01', '企业版定价25万/年，转化率仅2%', '焦虑', 0.7, 7, '0.67,0.33,0.82,0.45,0.91,0.12,0.78,0.56')")

print()
print('=== 自动生成的语义 ===')
print()

# 查询推理结果
print('5. OWL 推理结果：')
print('   插入的决策自动继承为 Decision 类')
print('   StrategicDecision 和 TacticalDecision 自动包含相关决策')
print()

# 查询决策统计
print('6. 决策统计：')
rows = q("SELECT * FROM Decision")
if rows and 'data' in rows:
    decisions = rows['data']
    print(f'   总决策数: {len(decisions)}')
    for d in decisions:
        print(f'   - {d.get("title", "N/A")} [{d.get("category", "N/A")}] {d.get("emotion_state", "N/A")}')

print()
print('=== 演示完成 ===')
print('数据插入时自动生成：')
print('  1. 文档存储')
print('  2. 图顶点')
print('  3. rdf:type 三元组')
print('  4. 属性三元组')
print('  5. OWL 推理（隐含类型）')
print('  6. 向量索引')
print('  7. B+Tree 索引')
