use serde_json::Value as JsonValue;
use crate::config::FieldFilter;

/// 语义向量自动生成器 — 从文档中提取文本并生成 embedding。
///
/// 负责：
/// 1. 根据 `FieldFilter` 策略从文档中提取文本字段
/// 2. 拼接为统一文本
/// 3. 调用 `EmbeddingModel` 生成向量
pub struct SemanticVectorizer {
    field_filter: FieldFilter,
}

impl SemanticVectorizer {
    /// 创建向量化器。
    pub fn new(field_filter: FieldFilter) -> Self {
        Self { field_filter }
    }

    /// 从文档中提取文本内容。
    ///
    /// 根据 `FieldFilter` 策略选择字段：
    /// - `AllString`：所有 STRING 类型字段（跳过 `__xxx__` 内部字段）
    /// - `Whitelist`：只使用白名单中的字段
    /// - `Blacklist`：排除指定字段
    /// - `None`：返回空字符串（禁用）
    ///
    /// 多个字段用 `; ` 拼接，格式为 `key: value`。
    pub fn extract_text(&self, doc: &serde_json::Map<String, JsonValue>) -> String {
        match &self.field_filter {
            FieldFilter::None => String::new(),

            FieldFilter::AllString => {
                doc.iter()
                    .filter(|(k, _)| !is_internal_field(k))
                    .filter_map(|(k, v)| {
                        v.as_str().map(|s| format!("{}: {}", k, s))
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            }

            FieldFilter::Whitelist(fields) => {
                doc.iter()
                    .filter(|(k, _)| fields.contains(k))
                    .filter_map(|(k, v)| {
                        v.as_str().map(|s| format!("{}: {}", k, s))
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            }

            FieldFilter::Blacklist(excluded) => {
                doc.iter()
                    .filter(|(k, _)| !is_internal_field(k) && !excluded.contains(k))
                    .filter_map(|(k, v)| {
                        v.as_str().map(|s| format!("{}: {}", k, s))
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        }
    }

    /// 检查文档是否包含可向量化的文本内容。
    pub fn has_text_content(&self, doc: &serde_json::Map<String, JsonValue>) -> bool {
        !self.extract_text(doc).is_empty()
    }
}

/// 判断是否为 OntoDB 内部字段（以 `__` 开头和结尾）。
fn is_internal_field(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_text_all_string() {
        let v = SemanticVectorizer::new(FieldFilter::AllString);
        let mut doc = serde_json::Map::new();
        doc.insert("name".to_string(), json!("iPhone"));
        doc.insert("price".to_string(), json!(999));
        doc.insert("description".to_string(), json!("A smartphone"));
        doc.insert("__class__".to_string(), json!("Product"));

        let text = v.extract_text(&doc);
        assert!(text.contains("name: iPhone"));
        assert!(text.contains("description: A smartphone"));
        assert!(!text.contains("__class__"));
        // price 是 number，不包含
        assert!(!text.contains("price"));
    }

    #[test]
    fn test_extract_text_whitelist() {
        let v = SemanticVectorizer::new(FieldFilter::Whitelist(vec!["title".to_string()]));
        let mut doc = serde_json::Map::new();
        doc.insert("title".to_string(), json!("Hello"));
        doc.insert("body".to_string(), json!("World"));

        let text = v.extract_text(&doc);
        assert!(text.contains("title: Hello"));
        assert!(!text.contains("body"));
    }

    #[test]
    fn test_extract_text_blacklist() {
        let v = SemanticVectorizer::new(FieldFilter::Blacklist(vec!["secret".to_string()]));
        let mut doc = serde_json::Map::new();
        doc.insert("name".to_string(), json!("Alice"));
        doc.insert("secret".to_string(), json!("hidden"));

        let text = v.extract_text(&doc);
        assert!(text.contains("name: Alice"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn test_extract_text_none() {
        let v = SemanticVectorizer::new(FieldFilter::None);
        let mut doc = serde_json::Map::new();
        doc.insert("name".to_string(), json!("test"));

        assert!(v.extract_text(&doc).is_empty());
    }

    #[test]
    fn test_internal_field() {
        assert!(is_internal_field("__class__"));
        assert!(is_internal_field("__pk__"));
        assert!(!is_internal_field("name"));
        assert!(!is_internal_field("_private"));
    }
}
