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
                return False
            return True
    except:
        return False

stmts = [
    # 更多决策记录
    "INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('是否自研AI大模型', '评估自研大模型的投入产出比，还是接入第三方API', '战略', 'P1', '已决策', '先接入第三方API快速上线，同时组建3人小团队预研自研模型，6个月后评估是否切换', '2026-06-01', '大模型热潮，竞对都在推AI功能，团队有ML基础但缺乏大模型训练经验', '犹豫', 0.55, 7, '0.67,0.33,0.82,0.45,0.91,0.12,0.78,0.56')",
    "INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('核心架构师离职应对', '核心技术骨干提出离职，如何保证产品迭代不受影响', '人事', 'P0', '已执行', '启动知识转移计划，2周内完成核心模块文档化；同时启动招聘，目标1个月内到岗；临时由副手接管', '2026-04-15', '架构师是OntoDB内核主要贡献者，掌握核心算法实现细节', '焦虑', 0.6, 9, '0.45,0.78,0.33,0.91,0.56,0.67,0.82,0.23')",
    "INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('是否接受政务大单', '某省政府提出定制化需求，金额2000万但需要大量定制开发', '产品', 'P0', '已执行', '接受订单，但要求分3期交付，首期聚焦核心需求，后期需求纳入标准产品迭代', '2026-07-01', '政务客户预算充足但需求复杂，团队产能有限', '兴奋', 0.7, 8, '0.82,0.45,0.67,0.33,0.91,0.56,0.78,0.12')",
    "INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('技术路线选择：Rust vs Go', '新模块开发语言选择，Rust性能好但招人难，Go生态好但性能略逊', '产品', 'P1', '已决策', '坚持Rust，核心优势在于内存安全和性能，这是我们的技术护城河；同时降低招聘门槛，接受有C++经验的候选人转Rust', '2025-11-01', '团队Rust经验不足，招聘困难，部分成员建议换Go', '坚定', 0.85, 5, '0.78,0.56,0.45,0.82,0.33,0.67,0.91,0.12')",
    "INSERT INTO Decision (title, description, category, priority, status, decision_text, decision_date, context_summary, emotion_state, confidence_score, pressure_level, embedding) VALUES ('开源策略调整', '是否将核心引擎开源以获取社区关注', '战略', 'P1', '已执行', '核心引擎Apache 2.0开源，企业版闭源；通过开源获取开发者社区关注和贡献，企业版提供增值功能变现', '2025-08-01', 'TiDB、CockroachDB等竞品都走开源路线，社区影响力是技术品牌的关键', '冷静', 0.75, 4, '0.67,0.45,0.82,0.33,0.56,0.78,0.91,0.23')",

    # 决策上下文
    "INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D005', '市场变化', '大模型热潮席卷行业，客户开始要求AI功能', '震荡', '竞对已推出AI助手功能', 6, 5, 7)",
    "INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D006', '内部危机', '核心架构师突然提出离职，团队士气受影响', '平稳', '竞对可能趁机挖角', 3, 5, 9)",
    "INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D007', '市场变化', '政务数字化加速，大额订单涌现', '平稳', '竞对也在争夺政务市场', 7, 4, 7)",
    "INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D008', '内部危机', '团队对技术路线产生分歧，部分成员倾向Go', '平稳', 'Go生态在云原生领域占优', 5, 4, 3)",
    "INSERT INTO DecisionContext (decision_id, event_type, event_detail, market_condition, competition, team_morale, financial_pressure, time_pressure) VALUES ('D009', '市场变化', '开源数据库市场快速增长，社区影响力成为竞争关键', '牛市', 'TiDB/CockroachDB开源策略成功', 7, 5, 5)",

    # 决策步骤
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D005', 1, '大模型是趋势，但自研成本高风险大', '自研/接入第三方/混合', '混合方案最稳妥：快速上线+长期布局', '犹豫', '2026-05-25')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D005', 2, '先接入第三方API验证市场需求', '百度/阿里/自研', '选择百度文心一言，API稳定且有中文优势', '冷静', '2026-05-28')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D005', 3, '同时组建小团队预研，为未来切换做准备', '3人/5人/8人', '3人精干团队，专注核心技术预研', '兴奋', '2026-06-01')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D006', 1, '架构师离职影响大，必须快速响应', '挽留/交接/两者并行', '先尝试挽留，同时启动知识转移', '焦虑', '2026-04-10')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D006', 2, '知识转移是关键，核心算法不能断档', '文档化/代码审查/结对编程', '文档化+代码审查双管齐下', '冷静', '2026-04-12')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D006', 3, '招聘要快，但不能降低标准', '内部提拔/外部招聘/猎头', '三管齐下，内部提拔优先', '坚定', '2026-04-15')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D007', 1, '政务大单是机会也是挑战', '接受/拒绝/部分接受', '分3期交付降低风险', '兴奋', '2026-06-20')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, reason, emotion, timestamp) VALUES ('D007', 2, '首期聚焦核心需求，后期纳入标准产品', '全部定制/分3期/拒绝', '分3期最平衡，既拿下订单又不偏离产品路线', '冷静', '2026-06-25')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D008', 1, 'Rust是我们的技术护城河，不能放弃', '坚持Rust/换Go/混合', 'Rust的内存安全和性能是核心竞争力', '坚定', '2025-10-15')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D008', 2, '招聘难就降低门槛，接受C++转Rust', '只招Rust/接受C++/培训新人', '接受C++经验者转Rust，扩大人才池', '冷静', '2025-10-20')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D009', 1, '开源是技术品牌的必经之路', '全开源/部分开源/闭源', '核心引擎开源，企业版闭源，双赢策略', '兴奋', '2025-07-15')",
    "INSERT INTO DecisionStep (decision_id, step_order, thought, option, reason, emotion, timestamp) VALUES ('D009', 2, 'Apache 2.0最友好，企业客户无顾虑', 'Apache/MIT/AGPL', 'Apache 2.0对企业最友好，无传染性', '冷静', '2025-07-20')",

    # 决策结果
    "INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D005', '部分成功', '第三方API快速上线获得客户认可，但自研进度滞后', 200, 6, '混合策略是对的，但自研团队投入需要加大', '2026-12-01')",
    "INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D006', '成功', '知识转移完成，新架构师3个月到岗，产品迭代未受影响', 0, 8, '核心人才的知识转移要常态化，不能等离职才做', '2026-07-15')",
    "INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D007', '成功', '首期按期交付，客户满意度高，二期合同已签订', 2000, 7, '政务大单要分阶段交付，降低风险', '2027-01-01')",
    "INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D008', '成功', 'Rust团队稳定，性能优势成为核心卖点', 0, 9, '技术路线选择要坚持长期主义', '2026-11-01')",
    "INSERT INTO DecisionOutcome (decision_id, result_type, result_detail, revenue_impact, team_impact, lesson_learned, review_date) VALUES ('D009', '成功', 'GitHub star 2000+，社区贡献者30+，企业版转化率提升', 500, 7, '开源是最好的技术营销', '2026-08-01')",
]

ok = fail = 0
for sql in stmts:
    if execute(sql): ok += 1
    else: fail += 1
print(f"Done: {ok} OK, {fail} failed")
