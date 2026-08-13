"""
OntoQL 继承查询 Demo

演示双螺旋架构的核心优势：
- TBox (本体): 定义类继承关系
- ABox (实例): 存储具体数据
- OntoQL: 一句查询自动展开继承层次

对比传统 SQL 需要手动 JOIN + 应用层 if-else 的复杂度。
"""

import requests
import json
import time

BASE_URL = "http://localhost:7912"

def api_post(endpoint, data):
    resp = requests.post(f"{BASE_URL}{endpoint}", json=data, timeout=30)
    return resp.json()

def api_query(query):
    return api_post("/api/query", {"query": query})

def print_section(title):
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}")

def print_json(data):
    print(json.dumps(data, indent=2, ensure_ascii=False))

# ============================================================
# 1. 创建本体 (TBox) - 定义类继承关系
# ============================================================
print_section("1. 创建本体 (TBox)")

# 先清理
api_query("DROP CLASS 温度传感器 IF EXISTS")
api_query("DROP CLASS 湿度传感器 IF EXISTS")
api_query("DROP CLASS 空调 IF EXISTS")
api_query("DROP CLASS 智能灯 IF EXISTS")
api_query("DROP CLASS 传感器 IF EXISTS")
api_query("DROP CLASS 设备 IF EXISTS")

# 创建基类：设备
print("\n→ 创建基类: 设备")
result = api_query("""
    CREATE CLASS 设备 (
        id STRING,
        name STRING,
        location STRING,
        status STRING
    )
""")
print(f"  结果: {result.get('status', 'ok')}")

# 创建子类：传感器 (继承设备)
print("\n→ 创建子类: 传感器 (继承 设备)")
result = api_query("""
    CREATE CLASS 传感器 (
        温度 FLOAT,
        湿度 FLOAT,
        精度 FLOAT
    ) EXTENDS 设备
""")
print(f"  结果: {result.get('status', 'ok')}")

# 创建具体子类：温度传感器 (继承传感器)
print("\n→ 创建具体子类: 温度传感器 (继承 传感器)")
result = api_query("""
    CREATE CLASS 温度传感器 (
        量程_低 FLOAT,
        量程_高 FLOAT
    ) EXTENDS 传感器
""")
print(f"  结果: {result.get('status', 'ok')}")

# 创建具体子类：湿度传感器 (继承传感器)
print("\n→ 创建具体子类: 湿度传感器 (继承 传感器)")
result = api_query("""
    CREATE CLASS 湿度传感器 (
        防水等级 STRING
    ) EXTENDS 传感器
""")
print(f"  结果: {result.get('status', 'ok')}")

# 创建另一条继承链：空调 (继承设备)
print("\n→ 创建另一条继承链: 空调 (继承 设备)")
result = api_query("""
    CREATE CLASS 空调 (
        温度 FLOAT,
        模式 STRING,
        风速 INT
    ) EXTENDS 设备
""")
print(f"  结果: {result.get('status', 'ok')}")

# 创建智能灯 (继承设备，但没有温度属性)
print("\n→ 创建: 智能灯 (继承 设备，无温度属性)")
result = api_query("""
    CREATE CLASS 智能灯 (
        亮度 INT,
        色温 INT
    ) EXTENDS 设备
""")
print(f"  结果: {result.get('status', 'ok')}")

# ============================================================
# 2. 插入实例数据 (ABox)
# ============================================================
print_section("2. 插入实例数据 (ABox)")

devices = [
    # 温度传感器
    ("温度传感器", "T001", "机房温度传感器A", "机房", "正常", 35.2, 60.0, 0.1, -40.0, 80.0),
    ("温度传感器", "T002", "机房温度传感器B", "机房", "正常", 28.5, 45.0, 0.2, -40.0, 80.0),
    ("温度传感器", "T003", "仓库温度传感器", "仓库", "正常", 32.1, 55.0, 0.5, -20.0, 60.0),
    ("温度传感器", "T004", "冷库温度传感器", "冷库", "正常", -5.0, 80.0, 0.3, -30.0, 30.0),
    
    # 湿度传感器
    ("湿度传感器", "H001", "机房湿度传感器A", "机房", "正常", 31.5, 65.0, 0.2, "IP65"),
    ("湿度传感器", "H002", "机房湿度传感器B", "机房", "正常", 29.0, 70.0, 0.3, "IP65"),
    ("湿度传感器", "H003", "仓库湿度传感器", "仓库", "正常", 33.0, 45.0, 0.5, "IP67"),
    
    # 空调 (也有温度属性！)
    ("空调", "AC001", "机房空调A", "机房", "运行", 35.0, None, None, None, "制冷", 3),
    ("空调", "AC002", "机房空调B", "机房", "运行", 22.0, None, None, None, "制热", 2),
    ("空调", "AC003", "办公室空调", "办公室", "关闭", 26.0, None, None, None, "自动", 1),
    
    # 智能灯 (没有温度属性)
    ("智能灯", "L001", "机房灯A", "机房", "开启", None, None, None, 80, 4000),
    ("智能灯", "L002", "办公室灯", "办公室", "关闭", None, None, None, 0, 3000),
]

for device in devices:
    class_name = device[0]
    if class_name == "温度传感器":
        query = f"""
            INSERT INTO 温度传感器 SET 
                id = '{device[1]}', 
                name = '{device[2]}', 
                location = '{device[3]}', 
                status = '{device[4]}',
                温度 = {device[5]}, 
                湿度 = {device[6]}, 
                精度 = {device[7]},
                量程_低 = {device[8]}, 
                量程_高 = {device[9]}
        """
    elif class_name == "湿度传感器":
        query = f"""
            INSERT INTO 湿度传感器 SET 
                id = '{device[1]}', 
                name = '{device[2]}', 
                location = '{device[3]}', 
                status = '{device[4]}',
                温度 = {device[5]}, 
                湿度 = {device[6]}, 
                精度 = {device[7]},
                防水等级 = '{device[8]}'
        """
    elif class_name == "空调":
        query = f"""
            INSERT INTO 空调 SET 
                id = '{device[1]}', 
                name = '{device[2]}', 
                location = '{device[3]}', 
                status = '{device[4]}',
                温度 = {device[5]}, 
                模式 = '{device[8]}', 
                风速 = {device[9]}
        """
    elif class_name == "智能灯":
        query = f"""
            INSERT INTO 智能灯 SET 
                id = '{device[1]}', 
                name = '{device[2]}', 
                location = '{device[3]}', 
                status = '{device[4]}',
                亮度 = {device[8]}, 
                色温 = {device[9]}
        """
    
    result = api_query(query)
    print(f"  插入 {class_name}: {device[2]} -> {result.get('status', 'ok')}")

# ============================================================
# 3. 传统 SQL 方式 (需要手动 JOIN + 应用层逻辑)
# ============================================================
print_section("3. 传统 SQL 方式 (假设用关系型数据库)")

print("""
假设用 MySQL/PostgreSQL 实现同样的查询：

-- 需要 4 张表：devices, sensors, temperature_sensors, humidity_sensors, air_conditioners

-- 查询所有"温度 > 30 的传感器"，需要：

SELECT d.id, d.name, d.location, s.温度, s.湿度,
       CASE 
           WHEN ts.id IS NOT NULL THEN '温度传感器'
           WHEN hs.id IS NOT NULL THEN '湿度传感器'
           WHEN ac.id IS NOT NULL THEN '空调'
           ELSE '未知'
       END as 设备类型
FROM devices d
JOIN sensors s ON d.id = s.device_id
LEFT JOIN temperature_sensors ts ON s.id = ts.sensor_id
LEFT JOIN humidity_sensors hs ON s.id = hs.sensor_id
LEFT JOIN air_conditioners ac ON d.id = ac.device_id
WHERE s.温度 > 30
  AND d.status = '正常';

-- 应用层还需要额外逻辑：
-- 1. 判断设备类型，返回不同字段
-- 2. 处理 NULL 值
-- 3. 如果要加新的传感器类型，需要改 SQL + 应用代码
""")

# ============================================================
# 4. OntoQL 方式 (双螺旋架构)
# ============================================================
print_section("4. OntoQL 方式 (双螺旋架构)")

print("\n→ 查询: 所有温度 > 30 的传感器")
print("  OntoQL: SELECT * FROM 传感器 WHERE 温度 > 30\n")

result = api_query("SELECT * FROM 传感器 WHERE 温度 > 30")
print("结果:")
if 'rows' in result:
    for row in result['rows']:
        print(f"  - {row.get('name', 'N/A')}: 温度={row.get('温度', 'N/A')}°C, "
              f"类型={row.get('__class__', 'N/A')}, 位置={row.get('location', 'N/A')}")
elif 'data' in result:
    for row in result['data']:
        print(f"  - {row.get('name', 'N/A')}: 温度={row.get('温度', 'N/A')}°C, "
              f"类型={row.get('__class__', 'N/A')}, 位置={row.get('location', 'N/A')}")
else:
    print_json(result)

print("\n→ 查询: 所有温度 > 30 且状态正常的传感器")
print("  OntoQL: SELECT * FROM 传感器 WHERE 温度 > 30 AND status = '正常'\n")

result = api_query("SELECT * FROM 传感器 WHERE 温度 > 30 AND status = '正常'")
print("结果:")
if 'rows' in result:
    for row in result['rows']:
        print(f"  - {row.get('name', 'N/A')}: 温度={row.get('温度', 'N/A')}°C, "
              f"类型={row.get('__class__', 'N/A')}")
elif 'data' in result:
    for row in result['data']:
        print(f"  - {row.get('name', 'N/A')}: 温度={row.get('温度', 'N/A')}°C, "
              f"类型={row.get('__class__', 'N/A')}")
else:
    print_json(result)

print("\n→ 查询: 所有设备 (包括没有温度属性的)")
print("  OntoQL: SELECT * FROM 设备\n")

result = api_query("SELECT * FROM 设备")
print("结果:")
if 'rows' in result:
    for row in result['rows']:
        temp_info = f", 温度={row.get('温度', 'N/A')}°C" if '温度' in row else ""
        print(f"  - {row.get('name', 'N/A')}: 类型={row.get('__class__', 'N/A')}{temp_info}")
elif 'data' in result:
    for row in result['data']:
        temp_info = f", 温度={row.get('温度', 'N/A')}°C" if '温度' in row else ""
        print(f"  - {row.get('name', 'N/A')}: 类型={row.get('__class__', 'N/A')}{temp_info}")
else:
    print_json(result)

# ============================================================
# 5. 高级查询示例
# ============================================================
print_section("5. 高级查询示例")

print("\n→ 按位置统计传感器数量")
print("  OntoQL: SELECT location, COUNT(*) as cnt FROM 传感器 GROUP BY location\n")

result = api_query("SELECT location, COUNT(*) as cnt FROM 传感器 GROUP BY location")
print("结果:")
print_json(result)

print("\n→ 查询温度最高且在机房的传感器")
print("  OntoQL: SELECT * FROM 传感器 WHERE location = '机房' ORDER BY 温度 DESC LIMIT 3\n")

result = api_query("SELECT * FROM 传感器 WHERE location = '机房' ORDER BY 温度 DESC LIMIT 3")
print("结果:")
if 'rows' in result:
    for i, row in enumerate(result['rows'], 1):
        print(f"  {i}. {row.get('name', 'N/A')}: {row.get('温度', 'N/A')}°C")
elif 'data' in result:
    for i, row in enumerate(result['data'], 1):
        print(f"  {i}. {row.get('name', 'N/A')}: {row.get('温度', 'N/A')}°C")
else:
    print_json(result)

# ============================================================
# 6. 总结对比
# ============================================================
print_section("6. 总结对比")

print("""
┌─────────────────────────────────────────────────────────────────────┐
│                        传统 SQL vs OntoQL                          │
├─────────────────────────────────────────────────────────────────────┤
│  需求: 查询所有温度 > 30 的传感器                                    │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  【传统 SQL】                                                        │
│  ───────────                                                        │
│  1. 设计 4+ 张表 (devices, sensors, temperature_sensors, ...)       │
│  2. 写复杂的 JOIN + LEFT JOIN SQL                                   │
│  3. 应用层写 if-else 判断设备类型                                    │
│  4. 处理 NULL 值和类型转换                                           │
│  5. 新增传感器类型 → 改表结构 + SQL + 应用代码                        │
│                                                                     │
│  【OntoQL】                                                          │
│  ─────────                                                          │
│  1. 本体定义继承关系 (一次性)                                        │
│  2. 一句查询: SELECT * FROM 传感器 WHERE 温度 > 30                   │
│  3. 内核自动展开继承层次，查询所有子类                                │
│  4. 自动返回所有属性，包括继承的                                     │
│  5. 新增传感器类型 → 只需 EXTENDS 传感器，查询无需修改                │
│                                                                     │
├─────────────────────────────────────────────────────────────────────┤
│  【优势】                                                            │
│  ✓ 代码量: 1 行 vs 20+ 行                                           │
│  ✓ 可维护性: 本体变更，查询自动适应                                  │
│  ✓ 类型安全: TBox 保证数据一致性                                     │
│  ✓ 语义明确: "传感器" 是本体概念，不是物理表                         │
└─────────────────────────────────────────────────────────────────────┘
""")

print("\nDemo 完成！")
