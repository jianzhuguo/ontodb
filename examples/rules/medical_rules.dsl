# Copyright (c) 2024-2026 OntoDB Team
# Licensed under the Business Source License 1.1 (BUSL-1.1).
# See LICENSE for details. Change Date: 2031-09-15.

# ============================================================
# 医疗健康规则集 — 业务专家直接编写
# ============================================================

# 规则1：高血压预警
RULE: 高血压预警
ID: med_001
WHEN:
    患者.收缩压 > 140
    患者.舒张压 > 90
THEN:
    患者.风险等级 = "高血压"
    通知.级别 = "高"
PRIORITY: 高
ENABLED: true

# 规则2：血糖异常
RULE: 血糖异常预警
ID: med_002
WHEN:
    患者.空腹血糖 > 7.0
THEN:
    患者.风险等级 = "糖尿病风险"
    通知.级别 = "中"
PRIORITY: 中
ENABLED: true

# 规则3：心率异常
RULE: 心率过快
ID: med_003
WHEN:
    患者.心率 > 100
THEN:
    患者.风险等级 = "心动过速"
    通知.级别 = "高"
PRIORITY: 高
ENABLED: true

# 规则4：综合健康评估
RULE: 综合健康良好
ID: med_004
WHEN:
    患者.收缩压 < 120
    患者.心率 < 80
    患者.空腹血糖 < 5.6
THEN:
    患者.健康状态 = "良好"
PRIORITY: 低
ENABLED: true
