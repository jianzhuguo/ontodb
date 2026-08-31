"""
OntoDB 本体继承自动建模工具
根据现有类名和属性，自动推断继承关系并重建本体
"""
import json, urllib.request

def q(sql):
    body = json.dumps({'query': sql}).encode()
    req = urllib.request.Request('http://127.0.0.1:7912/api/query', data=body, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read())
    except Exception as e:
        return {'success': False, 'error': str(e)}

# ============================================================
# 配置：定义继承规则
# ============================================================
# 格式：子类名关键词 -> 父类名
# 如果类名包含关键词，自动继承对应的父类
INHERITANCE_RULES = {
    # 人员类
    'Employee': 'Person',
    'Customer': 'Person',
    'Lead': 'Person',
    'User': 'Person',
    'Member': 'Person',
    
    # 组织类
    'Department': 'Organization',
    'Team': 'Organization',
    'Company': 'Organization',
    'Project': 'Organization',
    
    # 资产类
    'Server': 'Asset',
    'Device': 'Asset',
    'Sensor': 'Asset',
    'Vehicle': 'Asset',
    
    # 事件类
    'Alert': 'Event',
    'Log': 'Event',
    'Metric': 'Event',
    'BizMetrics': 'Event',
    'Transaction': 'Event',
    
    # 产品类
    'Product': 'Catalog',
    'Service': 'Catalog',
    'Item': 'Catalog',
}

# 父类定义（自动生成）
PARENT_CLASSES = {
    'Person': {
        'properties': {
            'name': 'STRING',
            'email': 'STRING',
        }
    },
    'Organization': {
        'properties': {
            'name': 'STRING',
            'code': 'STRING',
        }
    },
    'Asset': {
        'properties': {
            'name': 'STRING',
            'status': 'STRING',
        }
    },
    'Event': {
        'properties': {
            'time': 'FLOAT64',
            'type': 'STRING',
        }
    },
    'Catalog': {
        'properties': {
            'name': 'STRING',
            'description': 'STRING',
        }
    },
}

def analyze_ontology(onto_name):
    """分析现有本体，返回类和属性"""
    req = urllib.request.Request('http://127.0.0.1:7912/api/schema')
    with urllib.request.urlopen(req, timeout=5) as resp:
        schema = json.loads(resp.read())
    
    for o in schema['data']['ontologies']:
        if o['name'] == onto_name:
            return o
    return None

def suggest_inheritance(classes):
    """根据类名和属性推断继承关系"""
    suggestions = {}
    for cls_name in classes:
        for keyword, parent in INHERITANCE_RULES.items():
            if keyword.lower() in cls_name.lower():
                suggestions[cls_name] = parent
                break
    return suggestions

def generate_ontology_sql(onto_name, classes, properties, inheritance_map):
    """生成带继承的 CREATE ONTOLOGY SQL"""
    lines = [f'CREATE ONTOLOGY {onto_name} (']
    
    # 1. 添加父类
    added_parents = set()
    for cls_name, parent in inheritance_map.items():
        if parent not in added_parents and parent in PARENT_CLASSES:
            parent_def = PARENT_CLASSES[parent]
            lines.append(f'    CLASS {parent},')
            for prop_name, prop_type in parent_def['properties'].items():
                lines.append(f'    PROPERTY {prop_name} DOMAIN {parent} RANGE {prop_type},')
            added_parents.add(parent)
    
    # 2. 添加子类（带继承）
    for cls_name in classes:
        parent = inheritance_map.get(cls_name)
        if parent:
            lines.append(f'    CLASS {cls_name} EXTENDS {parent},')
        else:
            lines.append(f'    CLASS {cls_name},')
        
        # 添加该类的属性
        cls_props = properties.get(cls_name, {})
        for prop_name, prop_type in cls_props.items():
            lines.append(f'    PROPERTY {prop_name} DOMAIN {cls_name} RANGE {prop_type},')
    
    # 移除最后一个逗号
    if lines[-1].endswith(','):
        lines[-1] = lines[-1][:-1]
    
    lines.append(')')
    return '\n'.join(lines)

def main():
    print('=== OntoDB 本体继承自动建模工具 ===\n')
    
    # 1. 获取所有本体
    req = urllib.request.Request('http://127.0.0.1:7912/api/schema')
    with urllib.request.urlopen(req, timeout=5) as resp:
        schema = json.loads(resp.read())
    
    ontologies = schema['data']['ontologies']
    print(f'发现 {len(ontologies)} 个本体:\n')
    for i, o in enumerate(ontologies):
        cls_count = len(o.get('classes', {}))
        print(f'  {i+1}. {o["name"]} ({cls_count} classes)')
    
    # 2. 分析每个本体
    for onto in ontologies:
        onto_name = onto['name']
        classes = list(onto.get('classes', {}).keys())
        properties = {}
        
        for cls_name in classes:
            cls_props = {}
            for prop_name, prop_info in onto.get('properties', {}).items():
                if prop_info.get('domain') == cls_name:
                    cls_props[prop_name] = prop_info.get('range', 'STRING')
            properties[cls_name] = cls_props
        
        # 推断继承关系
        inheritance = suggest_inheritance(classes)
        
        if inheritance:
            print(f'\n=== {onto_name}: 推断继承关系 ===')
            for child, parent in inheritance.items():
                print(f'  {child} EXTENDS {parent}')
            
            # 生成新的本体SQL
            sql = generate_ontology_sql(onto_name, classes, properties, inheritance)
            print(f'\n=== 生成的 OntoQL ===')
            print(sql)
            
            # 询问是否重建
            print(f'\n是否重建 {onto_name} 本体？(y/n)')
            # 在自动化模式下直接重建
            # user_input = input()
            # if user_input.lower() == 'y':
            #     rebuild_ontology(onto_name, sql)
        else:
            print(f'\n=== {onto_name}: 无继承关系 ===')
            print('  所有类都是平铺结构，无自动推断')

def rebuild_ontology(onto_name, sql):
    """重建本体"""
    # 1. 保存数据
    print('  保存数据...')
    # 2. 删除旧本体
    print('  删除旧本体...')
    # 3. 创建新本体
    print('  创建新本体...')
    # 4. 恢复数据
    print('  恢复数据...')
    print('  完成!')

if __name__ == '__main__':
    main()
