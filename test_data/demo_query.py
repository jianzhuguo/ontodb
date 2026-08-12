import urllib.request, json, time

API = 'http://127.0.0.1:7912/api/query'

def q(sql):
    time.sleep(0.05)
    d = json.dumps({'query': sql}).encode()
    r = urllib.request.Request(API, data=d, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(r, timeout=10) as resp:
            result = json.loads(resp.read())
            return result.get('data', [])
    except Exception as e:
        print(f'  ERR: {e}')
        return []

print('=' * 60)
print('  数字军师：语义查询演示')
print('=' * 60)
print()

# 1. 按情绪状态查询
print('1. 按情绪状态查询（焦虑的决策）：')
print('-' * 40)
rows = q("SELECT title, category, emotion_state, pressure_level FROM Decision WHERE emotion_state = '焦虑'")
for r in rows:
    print(f'  {r.get("title", "")} | {r.get("category", "")} | 压力: {r.get("pressure_level", "")}')
print()

# 2. 高压力决策查询
print('2. 高压力决策（压力 >= 7）：')
print('-' * 40)
rows = q("SELECT title, emotion_state, pressure_level, confidence_score FROM Decision WHERE pressure_level >= 7 ORDER BY pressure_level DESC")
for r in rows:
    print(f'  {r.get("title", "")} | {r.get("emotion_state", "")} | 压力: {r.get("pressure_level", "")} | 信心: {r.get("confidence_score", "")}')
print()

# 3. 按类别统计
print('3. 决策类别统计：')
print('-' * 40)
rows = q("SELECT category, COUNT(*) as count FROM Decision GROUP BY category")
for r in rows:
    print(f'  {r.get("category", "")}: {r.get("count", 0)} 条')
print()

# 4. 决策结果分析
print('4. 决策结果分析：')
print('-' * 40)
rows = q("SELECT d.title, o.result_type, o.revenue_impact, o.lesson_learned FROM Decision d JOIN DecisionOutcome o ON d.decision_id = o.decision_id")
for r in rows:
    print(f'  {r.get("title", "")}')
    print(f'    结果: {r.get("result_type", "")} | 收入影响: {r.get("revenue_impact", "")}万')
    print(f'    教训: {r.get("lesson_learned", "")}')
print()

# 5. 人格特征分析
print('5. 决策者人格特征：')
print('-' * 40)
rows = q("SELECT trait_name, trait_value FROM PersonalityTrait")
for r in rows:
    name = r.get('trait_name', '')
    value = r.get('trait_value', 0)
    bar = '█' * int(value * 20) + '░' * (20 - int(value * 20))
    print(f'  {name:12s} [{bar}] {value*100:.0f}%')
print()

# 6. 思维路径回溯
print('6. 决策思维路径（是否进入政务市场）：')
print('-' * 40)
rows = q("SELECT step_order, thought, reason, emotion FROM DecisionStep WHERE decision_id = 'D001' ORDER BY step_order")
for r in rows:
    print(f'  步骤{r.get("step_order", "")}: {r.get("thought", "")}')
    print(f'    → {r.get("reason", "")} [{r.get("emotion", "")}]')
print()

print('=' * 60)
print('  演示完成')
print('=' * 60)
