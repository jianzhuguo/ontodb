# Copyright (c) 2024-2026 OntoDB Team
# Licensed under the Business Source License 1.1 (BUSL-1.1).
# See LICENSE for details. Change Date: 2031-09-15.

# ============================================================
# 工业物联网规则集 — 业务专家直接编写，无需编程
# ============================================================
# 使用方法：
#   1. 将此文件保存为 .dsl 格式
#   2. 放入 OntoDB 的 rules/ 目录
#   3. 规则自动生效，无需重启数据库
# ============================================================

# 规则1：高温设备预警
RULE: 高温设备预警
ID: iot_001
WHEN:
    设备.温度 > 80
THEN:
    设备.状态 = "需维护"
    通知.级别 = "高"
PRIORITY: 高
ENABLED: true

# 规则2：设备长时间运行预警
RULE: 长时间运行预警
ID: iot_002
WHEN:
    设备.运行时间 > 24
    设备.温度 > 60
THEN:
    设备.状态 = "建议保养"
PRIORITY: 中
ENABLED: true

# 规则3：紧急停机
RULE: 紧急停机
ID: iot_003
WHEN:
    设备.温度 > 120
THEN:
    设备.状态 = "紧急停机"
    通知.级别 = "紧急"
    安全.动作 = "停机"
PRIORITY: 紧急
ENABLED: true

# 规则4：设备健康评分
RULE: 健康评分良好
ID: iot_004
WHEN:
    设备.温度 < 50
    设备.振动 < 5
    设备.运行时间 < 12
THEN:
    设备.健康评分 = "优秀"
PRIORITY: 低
ENABLED: true
