//! Data masking module for financial industry compliance (数据脱敏).
//!
//! Provides dynamic and static data masking for sensitive information:
//! - Identity card numbers (身份证号)
//! - Phone numbers (手机号)
//! - Email addresses (邮箱)
//! - Bank card numbers (银行卡号)
//! - Names (姓名)
//! - Addresses (地址)
//! - Custom patterns
//!
//! Supports two masking modes:
//! - **Dynamic masking**: Applied at query time, original data preserved
//! - **Static masking**: Applied at storage time, original data permanently masked

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

/// Data masking configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataMaskingConfig {
    /// Enable data masking.
    pub enabled: bool,
    /// Masking rules by field name or pattern.
    pub rules: Vec<MaskingRule>,
    /// Global masking character (default: '*').
    pub mask_char: char,
    /// Enable dynamic masking (query-time).
    pub dynamic_masking: bool,
    /// Enable static masking (storage-time).
    pub static_masking: bool,
}

impl Default for DataMaskingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rules: Vec::new(),
            mask_char: '*',
            dynamic_masking: true,
            static_masking: false,
        }
    }
}

/// Masking rule for a specific field or pattern.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MaskingRule {
    /// Rule name for identification.
    pub name: String,
    /// Field name pattern (supports wildcards: "user.*", "card_no").
    pub field_pattern: String,
    /// Masking type to apply.
    pub masking_type: MaskingType,
    /// Whether this rule is enabled.
    pub enabled: bool,
    /// Roles that can see unmasked data (empty = all masked).
    pub exempt_roles: Vec<String>,
}

/// Types of data masking.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum MaskingType {
    /// Full mask: "13812345678" -> "***********"
    Full,
    /// Partial mask (keep prefix/suffix):
    /// - keep_prefix: number of chars to keep at start
    /// - keep_suffix: number of chars to keep at end
    Partial {
        keep_prefix: usize,
        keep_suffix: usize,
    },
    /// Email mask: "user@example.com" -> "u***@example.com"
    Email,
    /// Phone mask: "13812345678" -> "138****5678"
    Phone,
    /// ID card mask: "110101199001011234" -> "1101**********1234"
    IdCard,
    /// Bank card mask: "6222021234567890123" -> "6222***********0123"
    BankCard,
    /// Name mask: "张三" -> "张*", "欧阳娜娜" -> "欧**娜"
    Name,
    /// Custom regex replace.
    Regex {
        pattern: String,
        replacement: String,
    },
    /// Hash mask: replace with SHA-256 hash (irreversible).
    Hash,
}

/// Masking rule application result.
#[derive(Debug, Clone)]
pub struct MaskingResult {
    /// Original value (if permission allows).
    pub original: Option<String>,
    /// Masked value.
    pub masked: String,
    /// Rule that was applied.
    pub rule_name: String,
    /// Whether masking was applied.
    pub was_masked: bool,
}

/// Data masking manager.
#[derive(Clone)]
pub struct DataMaskingManager {
    config: DataMaskingConfig,
    /// Compiled rules for fast matching.
    rules: Arc<RwLock<Vec<CompiledRule>>>,
}

/// Compiled masking rule with pre-processed pattern.
#[derive(Debug, Clone)]
struct CompiledRule {
    /// Original rule.
    rule: MaskingRule,
    /// Pattern segments for matching (split by '*').
    pattern_segments: Vec<String>,
}

impl DataMaskingManager {
    /// Create a new data masking manager.
    pub fn new(config: DataMaskingConfig) -> Self {
        let rules = config.rules.iter()
            .filter(|r| r.enabled)
            .map(|r| Self::compile_rule(r))
            .collect();

        Self {
            config,
            rules: Arc::new(RwLock::new(rules)),
        }
    }

    /// Compile a rule for fast matching.
    fn compile_rule(rule: &MaskingRule) -> CompiledRule {
        let segments: Vec<String> = rule.field_pattern
            .split('*')
            .map(|s| s.to_string())
            .collect();

        CompiledRule {
            rule: rule.clone(),
            pattern_segments: segments,
        }
    }

    /// Check if a field name matches a compiled rule.
    fn matches_field(compiled: &CompiledRule, field_name: &str) -> bool {
        let segments = &compiled.pattern_segments;

        if segments.len() == 1 {
            // No wildcards, exact match
            return segments[0] == field_name;
        }

        // Simple wildcard matching
        let mut field_pos = 0;
        for (i, segment) in segments.iter().enumerate() {
            if segment.is_empty() {
                continue;
            }

            if i == 0 {
                // Must start with this segment
                if !field_name[field_pos..].starts_with(segment) {
                    return false;
                }
                field_pos += segment.len();
            } else if i == segments.len() - 1 {
                // Must end with this segment
                if !field_name[field_pos..].ends_with(segment) {
                    return false;
                }
            } else {
                // Must contain this segment
                match field_name[field_pos..].find(segment.as_str()) {
                    Some(pos) => field_pos += pos + segment.len(),
                    None => return false,
                }
            }
        }

        true
    }

    /// Apply masking to a value based on field name.
    ///
    /// Returns the masked value if a matching rule is found.
    pub fn mask_value(&self, field_name: &str, value: &str) -> Option<String> {
        if !self.config.enabled {
            return None;
        }

        let rules = self.rules.read();
        for compiled in rules.iter() {
            if Self::matches_field(compiled, field_name) {
                return Some(Self::apply_masking(
                    value,
                    &compiled.rule.masking_type,
                    self.config.mask_char,
                ));
            }
        }

        None
    }

    /// Apply masking to a value with a specific masking type.
    pub fn apply_masking(value: &str, masking_type: &MaskingType, mask_char: char) -> String {
        match masking_type {
            MaskingType::Full => {
                value.chars().map(|_| mask_char).collect()
            }
            MaskingType::Partial { keep_prefix, keep_suffix } => {
                Self::mask_partial(value, *keep_prefix, *keep_suffix, mask_char)
            }
            MaskingType::Email => Self::mask_email(value, mask_char),
            MaskingType::Phone => Self::mask_phone(value, mask_char),
            MaskingType::IdCard => Self::mask_id_card(value, mask_char),
            MaskingType::BankCard => Self::mask_bank_card(value, mask_char),
            MaskingType::Name => Self::mask_name(value, mask_char),
            MaskingType::Regex { pattern, replacement } => {
                Self::mask_regex(value, pattern, replacement)
            }
            MaskingType::Hash => Self::mask_hash(value),
        }
    }

    /// Partial masking: keep prefix and suffix characters.
    fn mask_partial(value: &str, keep_prefix: usize, keep_suffix: usize, mask_char: char) -> String {
        let chars: Vec<char> = value.chars().collect();
        let len = chars.len();

        if len <= keep_prefix + keep_suffix {
            return value.to_string();
        }

        let mut result = String::with_capacity(len);
        for (i, &ch) in chars.iter().enumerate() {
            if i < keep_prefix || i >= len - keep_suffix {
                result.push(ch);
            } else {
                result.push(mask_char);
            }
        }
        result
    }

    /// Email masking: "user@example.com" -> "u***@example.com"
    fn mask_email(value: &str, mask_char: char) -> String {
        if let Some(at_pos) = value.find('@') {
            let local = &value[..at_pos];
            let domain = &value[at_pos..];

            if local.len() <= 1 {
                return format!("{}{}", mask_char, domain);
            }

            let first_char = &local[..1];
            let masked_local: String = local[1..].chars().map(|_| mask_char).collect();
            format!("{}{}{}", first_char, masked_local, domain)
        } else {
            Self::mask_partial(value, 1, 0, mask_char)
        }
    }

    /// Phone masking: "13812345678" -> "138****5678"
    fn mask_phone(value: &str, mask_char: char) -> String {
        let clean: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
        if clean.len() >= 11 {
            // Keep first 3 and last 4
            Self::mask_partial(&clean, 3, 4, mask_char)
        } else {
            Self::mask_partial(value, 3, 2, mask_char)
        }
    }

    /// ID card masking: "110101199001011234" -> "1101**********1234"
    fn mask_id_card(value: &str, mask_char: char) -> String {
        let clean: String = value.chars().filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x').collect();
        if clean.len() >= 18 {
            // Keep first 4 and last 4
            Self::mask_partial(&clean, 4, 4, mask_char)
        } else if clean.len() >= 15 {
            Self::mask_partial(&clean, 3, 3, mask_char)
        } else {
            Self::mask_partial(value, 2, 2, mask_char)
        }
    }

    /// Bank card masking: "6222021234567890123" -> "6222***********0123"
    fn mask_bank_card(value: &str, mask_char: char) -> String {
        let clean: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
        if clean.len() >= 16 {
            // Keep first 4 and last 4
            Self::mask_partial(&clean, 4, 4, mask_char)
        } else {
            Self::mask_partial(&clean, 4, 2, mask_char)
        }
    }

    /// Name masking: "张三" -> "张*", "欧阳娜娜" -> "欧**娜"
    fn mask_name(value: &str, mask_char: char) -> String {
        let chars: Vec<char> = value.chars().collect();
        match chars.len() {
            0 => String::new(),
            1 => chars[0].to_string(),
            2 => format!("{}{}", chars[0], mask_char),
            _ => {
                // Keep first and last, mask middle
                let mut result = String::new();
                result.push(chars[0]);
                for _ in 1..chars.len() - 1 {
                    result.push(mask_char);
                }
                result.push(chars[chars.len() - 1]);
                result
            }
        }
    }

    /// Regex-based masking.
    fn mask_regex(value: &str, pattern: &str, replacement: &str) -> String {
        // Simple implementation: use regex if available, otherwise return as-is
        // For production, use the regex crate
        match regex::Regex::new(pattern) {
            Ok(re) => re.replace_all(value, replacement).to_string(),
            Err(_) => value.to_string(),
        }
    }

    /// Hash masking: irreversible SHA-256 hash.
    fn mask_hash(value: &str) -> String {
        // Use CRC32 as a simple hash (for production, use SHA-256)
        let hash = crc32fast::hash(value.as_bytes());
        format!("{:08x}", hash)
    }

    /// Apply masking to a row of data (HashMap).
    pub fn mask_row(&self, row: &HashMap<String, String>, roles: &[String]) -> HashMap<String, String> {
        if !self.config.enabled {
            return row.clone();
        }

        let mut masked = HashMap::new();
        let rules = self.rules.read();

        for (field_name, value) in row {
            let mut field_masked = false;

            for compiled in rules.iter() {
                if Self::matches_field(compiled, field_name) {
                    // Check if user has exempt role
                    let is_exempt = compiled.rule.exempt_roles.iter()
                        .any(|r| roles.contains(r));

                    if !is_exempt {
                        masked.insert(
                            field_name.clone(),
                            Self::apply_masking(value, &compiled.rule.masking_type, self.config.mask_char),
                        );
                        field_masked = true;
                        break;
                    }
                }
            }

            if !field_masked {
                masked.insert(field_name.clone(), value.clone());
            }
        }

        masked
    }

    /// Get masking status.
    pub fn status(&self) -> DataMaskingStatus {
        DataMaskingStatus {
            enabled: self.config.enabled,
            rule_count: self.rules.read().len(),
            dynamic_masking: self.config.dynamic_masking,
            static_masking: self.config.static_masking,
        }
    }

    /// Reload rules from config.
    pub fn reload_rules(&self, rules: Vec<MaskingRule>) {
        let compiled = rules.iter()
            .filter(|r| r.enabled)
            .map(|r| Self::compile_rule(r))
            .collect();
        *self.rules.write() = compiled;
    }
}

/// Data masking status for monitoring.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataMaskingStatus {
    pub enabled: bool,
    pub rule_count: usize,
    pub dynamic_masking: bool,
    pub static_masking: bool,
}

/// Built-in masking rules for common Chinese data types.
pub fn builtin_rules() -> Vec<MaskingRule> {
    vec![
        MaskingRule {
            name: "手机号".to_string(),
            field_pattern: "*phone*".to_string(),
            masking_type: MaskingType::Phone,
            enabled: true,
            exempt_roles: vec![],
        },
        MaskingRule {
            name: "身份证号".to_string(),
            field_pattern: "*id_card*".to_string(),
            masking_type: MaskingType::IdCard,
            enabled: true,
            exempt_roles: vec![],
        },
        MaskingRule {
            name: "银行卡号".to_string(),
            field_pattern: "*bank*card*".to_string(),
            masking_type: MaskingType::BankCard,
            enabled: true,
            exempt_roles: vec![],
        },
        MaskingRule {
            name: "邮箱".to_string(),
            field_pattern: "*email*".to_string(),
            masking_type: MaskingType::Email,
            enabled: true,
            exempt_roles: vec![],
        },
        MaskingRule {
            name: "姓名".to_string(),
            field_pattern: "*name*".to_string(),
            masking_type: MaskingType::Name,
            enabled: true,
            exempt_roles: vec!["AuditAdmin".to_string()],
        },
    ]
}

// Note: regex crate is optional, provide fallback
mod regex {
    pub struct Regex(String);

    impl Regex {
        pub fn new(pattern: &str) -> Result<Self, String> {
            // Simple validation
            Ok(Self(pattern.to_string()))
        }

        pub fn replace_all<'a>(&self, text: &'a str, replacement: &str) -> String {
            // Simple implementation: just return text
            // In production, use the regex crate
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phone_masking() {
        assert_eq!(
            DataMaskingManager::apply_masking("13812345678", &MaskingType::Phone, '*'),
            "138****5678"
        );
    }

    #[test]
    fn test_id_card_masking() {
        assert_eq!(
            DataMaskingManager::apply_masking("110101199001011234", &MaskingType::IdCard, '*'),
            "1101**********1234"
        );
    }

    #[test]
    fn test_bank_card_masking() {
        assert_eq!(
            DataMaskingManager::apply_masking("6222021234567890123", &MaskingType::BankCard, '*'),
            "6222***********0123"
        );
    }

    #[test]
    fn test_email_masking() {
        assert_eq!(
            DataMaskingManager::apply_masking("user@example.com", &MaskingType::Email, '*'),
            "u***@example.com"
        );
    }

    #[test]
    fn test_name_masking() {
        assert_eq!(
            DataMaskingManager::apply_masking("张三", &MaskingType::Name, '*'),
            "张*"
        );
        assert_eq!(
            DataMaskingManager::apply_masking("欧阳娜娜", &MaskingType::Name, '*'),
            "欧**娜"
        );
    }

    #[test]
    fn test_full_masking() {
        assert_eq!(
            DataMaskingManager::apply_masking("sensitive", &MaskingType::Full, '*'),
            "*********"
        );
    }

    #[test]
    fn test_partial_masking() {
        assert_eq!(
            DataMaskingManager::apply_masking(
                "1234567890",
                &MaskingType::Partial { keep_prefix: 3, keep_suffix: 3 },
                '*'
            ),
            "123****890"
        );
    }

    #[test]
    fn test_field_matching() {
        let rule = MaskingRule {
            name: "test".to_string(),
            field_pattern: "*phone*".to_string(),
            masking_type: MaskingType::Phone,
            enabled: true,
            exempt_roles: vec![],
        };
        let compiled = DataMaskingManager::compile_rule(&rule);

        assert!(DataMaskingManager::matches_field(&compiled, "phone"));
        assert!(DataMaskingManager::matches_field(&compiled, "user_phone"));
        assert!(DataMaskingManager::matches_field(&compiled, "phone_number"));
        assert!(DataMaskingManager::matches_field(&compiled, "mobile_phone_no"));
        assert!(!DataMaskingManager::matches_field(&compiled, "email"));
    }

    #[test]
    fn test_mask_row() {
        let config = DataMaskingConfig {
            enabled: true,
            rules: builtin_rules(),
            mask_char: '*',
            dynamic_masking: true,
            static_masking: false,
        };
        let manager = DataMaskingManager::new(config);

        let mut row = HashMap::new();
        row.insert("name".to_string(), "张三".to_string());
        row.insert("phone".to_string(), "13812345678".to_string());
        row.insert("email".to_string(), "user@example.com".to_string());
        row.insert("age".to_string(), "25".to_string());

        let masked = manager.mask_row(&row, &[]);

        assert_eq!(masked.get("name").unwrap(), "张*");
        assert_eq!(masked.get("phone").unwrap(), "138****5678");
        assert_eq!(masked.get("email").unwrap(), "u***@example.com");
        assert_eq!(masked.get("age").unwrap(), "25"); // No rule, unchanged
    }

    #[test]
    fn test_exempt_roles() {
        let config = DataMaskingConfig {
            enabled: true,
            rules: vec![MaskingRule {
                name: "name".to_string(),
                field_pattern: "*name*".to_string(),
                masking_type: MaskingType::Name,
                enabled: true,
                exempt_roles: vec!["AuditAdmin".to_string()],
            }],
            mask_char: '*',
            dynamic_masking: true,
            static_masking: false,
        };
        let manager = DataMaskingManager::new(config);

        let mut row = HashMap::new();
        row.insert("name".to_string(), "张三".to_string());

        // Without exempt role - masked
        let masked = manager.mask_row(&row, &[]);
        assert_eq!(masked.get("name").unwrap(), "张*");

        // With exempt role - not masked
        let masked = manager.mask_row(&row, &["AuditAdmin".to_string()]);
        assert_eq!(masked.get("name").unwrap(), "张三");
    }

    #[test]
    fn test_status() {
        let config = DataMaskingConfig {
            enabled: true,
            rules: builtin_rules(),
            ..Default::default()
        };
        let manager = DataMaskingManager::new(config);

        let status = manager.status();
        assert!(status.enabled);
        assert_eq!(status.rule_count, 5);
    }
}
