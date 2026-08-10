import urllib.request, json, time

API = 'http://127.0.0.1:7912/api/query'

def execute(sql):
    time.sleep(0.03)
    data = json.dumps({"query": sql}).encode('utf-8')
    req = urllib.request.Request(API, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            result = json.loads(resp.read())
            if result.get("error"):
                print(f"  ERR: {result['error'][:100]}")
                return False
            return True
    except Exception as e:
        print(f"  FAIL: {e}")
        return False

stmts = [
    # ── OWL 本体定义 ──
    # 创建本体
    "INSERT INTO __ontologies__ (uri, prefix, description) VALUES ('http://ontodb.ai/ontology/enterprise', 'ent', 'OntoDB企业本体')",

    # OWL 类层次 (Class Hierarchy)
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Organization', '组织', NULL, 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Department', '部门', 'http://ontodb.ai/ontology/enterprise#Organization', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Team', '团队', 'http://ontodb.ai/ontology/enterprise#Department', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Person', '人员', NULL, 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Employee', '员工', 'http://ontodb.ai/ontology/enterprise#Person', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Manager', '管理者', 'http://ontodb.ai/ontology/enterprise#Employee', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Project', '项目', NULL, 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#ITProject', 'IT项目', 'http://ontodb.ai/ontology/enterprise#Project', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Asset', '资产', NULL, 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Server', '服务器', 'http://ontodb.ai/ontology/enterprise#Asset', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#CloudServer', '云服务器', 'http://ontodb.ai/ontology/enterprise#Server', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Customer', '客户', NULL, 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#VIPCustomer', 'VIP客户', 'http://ontodb.ai/ontology/enterprise#Customer', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#Contract', '合同', NULL, 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_classes__ (uri, label, parent_uri, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#ServiceContract', '服务合同', 'http://ontodb.ai/ontology/enterprise#Contract', 'http://ontodb.ai/ontology/enterprise')",

    # OWL 属性 (Properties)
    "INSERT INTO __ontology_properties__ (uri, label, domain_uri, range_uri, property_type, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#belongsTo', '属于', 'http://ontodb.ai/ontology/enterprise#Employee', 'http://ontodb.ai/ontology/enterprise#Department', 'object', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_properties__ (uri, label, domain_uri, range_uri, property_type, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#manages', '管理', 'http://ontodb.ai/ontology/enterprise#Manager', 'http://ontodb.ai/ontology/enterprise#Project', 'object', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_properties__ (uri, label, domain_uri, range_uri, property_type, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#hosts', '托管', 'http://ontodb.ai/ontology/enterprise#Server', 'http://ontodb.ai/ontology/enterprise#Project', 'object', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_properties__ (uri, label, domain_uri, range_uri, property_type, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#serves', '服务', 'http://ontodb.ai/ontology/enterprise#Department', 'http://ontodb.ai/ontology/enterprise#Customer', 'object', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_properties__ (uri, label, domain_uri, range_uri, property_type, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#signs', '签署', 'http://ontodb.ai/ontology/enterprise#Customer', 'http://ontodb.ai/ontology/enterprise#Contract', 'object', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_properties__ (uri, label, domain_uri, range_uri, property_type, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#hasSkill', '拥有技能', 'http://ontodb.ai/ontology/enterprise#Employee', NULL, 'data', 'http://ontodb.ai/ontology/enterprise')",
    "INSERT INTO __ontology_properties__ (uri, label, domain_uri, range_uri, property_type, ontology_uri) VALUES ('http://ontodb.ai/ontology/enterprise#budget', '预算', 'http://ontodb.ai/ontology/enterprise#Department', NULL, 'data', 'http://ontodb.ai/ontology/enterprise')",

    # Subproperty relationships (推理链)
    "INSERT INTO __rdfs_subproperty__ (sub_property_uri, super_property_uri) VALUES ('http://ontodb.ai/ontology/enterprise#manages', 'http://ontodb.ai/ontology/enterprise#belongsTo')",
    "INSERT INTO __rdfs_subproperty__ (sub_property_uri, super_property_uri) VALUES ('http://ontodb.ai/ontology/enterprise#signs', 'http://ontodb.ai/ontology/enterprise#serves')",

    # RDF type assertions (实体类型标注)
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Department/HQ', 'http://ontodb.ai/ontology/enterprise#Department')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Department/TECH', 'http://ontodb.ai/ontology/enterprise#Department')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Department/DBCORE', 'http://ontodb.ai/ontology/enterprise#Team')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Department/INFRA', 'http://ontodb.ai/ontology/enterprise#Team')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Department/FE', 'http://ontodb.ai/ontology/enterprise#Team')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Department/AI', 'http://ontodb.ai/ontology/enterprise#Team')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Employee/EMP001', 'http://ontodb.ai/ontology/enterprise#Manager')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Employee/EMP002', 'http://ontodb.ai/ontology/enterprise#Manager')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Employee/EMP003', 'http://ontodb.ai/ontology/enterprise#Manager')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Employee/EMP006', 'http://ontodb.ai/ontology/enterprise#Manager')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Employee/EMP008', 'http://ontodb.ai/ontology/enterprise#Manager')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Employee/EMP016', 'http://ontodb.ai/ontology/enterprise#Manager')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Server/N1', 'http://ontodb.ai/ontology/enterprise#Server')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Server/SG1', 'http://ontodb.ai/ontology/enterprise#CloudServer')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Server/DXB1', 'http://ontodb.ai/ontology/enterprise#CloudServer')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Customer/中国银行数据中心', 'http://ontodb.ai/ontology/enterprise#VIPCustomer')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Customer/中国人寿', 'http://ontodb.ai/ontology/enterprise#VIPCustomer')",
    "INSERT INTO __rdf_type__ (subject, class_uri) VALUES ('Customer/NEOM Tech', 'http://ontodb.ai/ontology/enterprise#VIPCustomer')",

    # ── 向量数据 (Embeddings) ──
    # 为实体创建向量表示
    "CREATE VERTEX TABLE EntityVector (entity_id STRING, entity_type STRING, vector STRING, cluster_id INT, description STRING)",
    # 部门向量
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('HQ', 'Department', '0.82,0.15,0.91,0.33,0.67', 0, '集团总部')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('TECH', 'Department', '0.78,0.82,0.45,0.91,0.23', 1, '技术研发中心')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('PRODUCT', 'Department', '0.65,0.71,0.88,0.42,0.55', 1, '产品事业部')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('SALES', 'Department', '0.45,0.33,0.72,0.15,0.89', 2, '销售中心')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('OVERSEAS', 'Department', '0.38,0.28,0.65,0.22,0.92', 2, '海外事业部')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('HR', 'Department', '0.55,0.42,0.78,0.35,0.61', 3, '人力资源部')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('FIN', 'Department', '0.62,0.48,0.82,0.28,0.58', 3, '财务部')",
    # 员工向量
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP001', 'Employee', '0.95,0.12,0.88,0.45,0.72', 0, 'CEO')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP002', 'Employee', '0.88,0.78,0.52,0.92,0.31', 1, 'CTO')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP008', 'Employee', '0.82,0.85,0.48,0.88,0.28', 1, '首席工程师')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP007', 'Employee', '0.75,0.82,0.42,0.85,0.35', 1, '架构师')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP016', 'Employee', '0.72,0.25,0.82,0.18,0.91', 2, '销售VP')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP029', 'Employee', '0.68,0.22,0.78,0.15,0.95', 2, '海外VP')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP003', 'Employee', '0.65,0.68,0.85,0.52,0.48', 3, 'VP产品')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('EMP010', 'Employee', '0.58,0.92,0.35,0.95,0.22', 1, '算法专家')",
    # 项目向量
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('PRJ001', 'Project', '0.75,0.88,0.42,0.91,0.35', 1, 'OntoDB内核v2.0')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('PRJ006', 'Project', '0.82,0.72,0.55,0.85,0.42', 1, '金融版合规引擎')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('PRJ005', 'Project', '0.68,0.55,0.78,0.48,0.62', 3, '政务版定制')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('PRJ013', 'Project', '0.55,0.42,0.65,0.38,0.82', 2, '新加坡智慧国')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('PRJ014', 'Project', '0.48,0.38,0.72,0.32,0.88', 2, '沙特NEOM')",
    # 客户向量
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('CUST0', 'Customer', '0.82,0.18,0.85,0.22,0.78', 0, '上海政务云')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('CUST1', 'Customer', '0.88,0.22,0.92,0.28,0.72', 0, '中国银行')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('CUST7', 'Customer', '0.85,0.25,0.88,0.25,0.75', 0, '中国人寿')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('CUST15', 'Customer', '0.42,0.35,0.58,0.32,0.92', 2, 'NEOM Tech')",
    "INSERT INTO EntityVector (entity_id, entity_type, vector, cluster_id, description) VALUES ('CUST16', 'Customer', '0.45,0.32,0.62,0.28,0.88', 2, 'Emirates NBD')",
]

ok = fail = 0
for sql in stmts:
    if execute(sql): ok += 1
    else: fail += 1
print(f"Done: {ok} OK, {fail} failed")
