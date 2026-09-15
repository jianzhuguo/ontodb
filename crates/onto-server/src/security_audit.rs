// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Security audit module for OntoDB.
//!
//! Performs automated security checks on the running system configuration,
//! similar to MySQL's `mysql_secure_installation` or PostgreSQL's `pg_audit`.
//!
//! Checks include:
//! - Authentication configuration
//! - TLS/SSL configuration
//! - Network exposure
//! - Password policies
//! - Default credentials
//! - File permissions
//! - Rate limiting
//! - Audit logging

use serde::{Deserialize, Serialize};

/// Severity level for security findings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// A single security finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityFinding {
    /// Check ID (e.g., "AUTH-001").
    pub id: String,
    /// Severity level.
    pub severity: Severity,
    /// Category (e.g., "Authentication", "TLS", "Network").
    pub category: String,
    /// Short title.
    pub title: String,
    /// Detailed description.
    pub description: String,
    /// Recommended remediation.
    pub remediation: String,
    /// Whether the check passed.
    pub passed: bool,
}

/// Security audit result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditResult {
    /// Timestamp of the audit.
    pub timestamp: String,
    /// Total checks performed.
    pub total_checks: usize,
    /// Number of passed checks.
    pub passed: usize,
    /// Number of failed checks.
    pub failed: usize,
    /// Findings grouped by severity.
    pub findings: Vec<SecurityFinding>,
    /// Overall security score (0-100).
    pub score: u32,
}

/// Configuration to audit.
#[derive(Debug, Clone)]
pub struct AuditTarget {
    pub auth_enabled: bool,
    pub api_keys_count: usize,
    pub tls_enabled: bool,
    pub tls_min_version: String,
    pub rate_limiting_enabled: bool,
    pub audit_logging_enabled: bool,
    pub cors_origins: String,
    pub has_default_credentials: bool,
    pub pgwire_enabled: bool,
    pub mysql_enabled: bool,
    pub exposed_ports: Vec<u16>,
}

/// Perform a security audit on the given configuration.
pub fn run_security_audit(target: &AuditTarget) -> SecurityAuditResult {
    let mut findings = Vec::new();

    // ── Authentication ──
    findings.push(check_auth_enabled(target));
    findings.push(check_api_keys_configured(target));
    findings.push(check_default_credentials(target));

    // ── TLS/SSL ──
    findings.push(check_tls_enabled(target));
    findings.push(check_tls_version(target));

    // ── Network ──
    findings.push(check_rate_limiting(target));
    findings.push(check_cors_config(target));
    findings.push(check_exposed_ports(target));

    // ── Logging & Monitoring ──
    findings.push(check_audit_logging(target));

    // ── Database Protocols ──
    findings.push(check_pgwire_auth(target));
    findings.push(check_mysql_auth(target));

    // Calculate score
    let total = findings.len();
    let passed = findings.iter().filter(|f| f.passed).count();
    let failed = total - passed;

    let score = if total == 0 {
        100
    } else {
        // Weighted scoring: Critical=0, High=10, Medium=5, Low=2, Info=1
        let max_score: u32 = findings.iter().map(|f| severity_weight(&f.severity)).sum();
        let earned: u32 = findings
            .iter()
            .filter(|f| f.passed)
            .map(|f| severity_weight(&f.severity))
            .sum();
        if max_score > 0 {
            (earned * 100) / max_score
        } else {
            100
        }
    };

    SecurityAuditResult {
        timestamp: chrono::Utc::now().to_rfc3339(),
        total_checks: total,
        passed,
        failed,
        findings,
        score,
    }
}

fn severity_weight(s: &Severity) -> u32 {
    match s {
        Severity::Critical => 20,
        Severity::High => 10,
        Severity::Medium => 5,
        Severity::Low => 2,
        Severity::Info => 1,
    }
}

// ── Individual Checks ──

fn check_auth_enabled(target: &AuditTarget) -> SecurityFinding {
    SecurityFinding {
        id: "AUTH-001".to_string(),
        severity: Severity::Critical,
        category: "Authentication".to_string(),
        title: "Authentication enabled".to_string(),
        description: "API key authentication should be enabled to prevent unauthorized access."
            .to_string(),
        remediation: "Start server with --auth flag and configure --api-keys-file.".to_string(),
        passed: target.auth_enabled,
    }
}

fn check_api_keys_configured(target: &AuditTarget) -> SecurityFinding {
    SecurityFinding {
        id: "AUTH-002".to_string(),
        severity: Severity::High,
        category: "Authentication".to_string(),
        title: "API keys configured".to_string(),
        description: "At least one API key should be configured.".to_string(),
        remediation: "Add API keys to the configuration file.".to_string(),
        passed: target.api_keys_count > 0,
    }
}

fn check_default_credentials(target: &AuditTarget) -> SecurityFinding {
    SecurityFinding {
        id: "AUTH-003".to_string(),
        severity: Severity::Critical,
        category: "Authentication".to_string(),
        title: "No default credentials".to_string(),
        description: "Default or CHANGE_ME credentials should not be used.".to_string(),
        remediation: "Replace all default passwords and keys with strong, unique values."
            .to_string(),
        passed: !target.has_default_credentials,
    }
}

fn check_tls_enabled(target: &AuditTarget) -> SecurityFinding {
    SecurityFinding {
        id: "TLS-001".to_string(),
        severity: Severity::High,
        category: "TLS/SSL".to_string(),
        title: "TLS enabled".to_string(),
        description: "TLS should be enabled for encrypted communication.".to_string(),
        remediation: "Configure --tls-cert and --tls-key to enable HTTPS.".to_string(),
        passed: target.tls_enabled,
    }
}

fn check_tls_version(target: &AuditTarget) -> SecurityFinding {
    let passed =
        !target.tls_enabled || target.tls_min_version == "1.2" || target.tls_min_version == "1.3";
    SecurityFinding {
        id: "TLS-002".to_string(),
        severity: Severity::Medium,
        category: "TLS/SSL".to_string(),
        title: "TLS minimum version 1.2+".to_string(),
        description: "TLS 1.0 and 1.1 should be disabled.".to_string(),
        remediation: "Set --tls-min-version to 1.2 or 1.3.".to_string(),
        passed,
    }
}

fn check_rate_limiting(target: &AuditTarget) -> SecurityFinding {
    SecurityFinding {
        id: "NET-001".to_string(),
        severity: Severity::Medium,
        category: "Network".to_string(),
        title: "Rate limiting enabled".to_string(),
        description: "Rate limiting should be enabled to prevent abuse.".to_string(),
        remediation: "Do not use --no-rate-limit flag.".to_string(),
        passed: target.rate_limiting_enabled,
    }
}

fn check_cors_config(target: &AuditTarget) -> SecurityFinding {
    let passed = target.cors_origins != "*";
    SecurityFinding {
        id: "NET-002".to_string(),
        severity: Severity::Medium,
        category: "Network".to_string(),
        title: "CORS not wildcard".to_string(),
        description: "CORS should not allow all origins (*) in production.".to_string(),
        remediation: "Set --cors-origins to specific allowed domains.".to_string(),
        passed,
    }
}

fn check_exposed_ports(target: &AuditTarget) -> SecurityFinding {
    let dangerous_ports: Vec<u16> = target
        .exposed_ports
        .iter()
        .filter(|p| **p == 0 || **p > 1024 || **p == 3306 || **p == 5432)
        .copied()
        .collect();
    let passed = dangerous_ports.is_empty() || dangerous_ports.iter().all(|p| *p >= 7900);
    SecurityFinding {
        id: "NET-003".to_string(),
        severity: Severity::Low,
        category: "Network".to_string(),
        title: "No unnecessary ports exposed".to_string(),
        description: "Database ports should not be exposed to the public internet.".to_string(),
        remediation: "Bind to 127.0.0.1 or use firewall rules.".to_string(),
        passed,
    }
}

fn check_audit_logging(target: &AuditTarget) -> SecurityFinding {
    SecurityFinding {
        id: "LOG-001".to_string(),
        severity: Severity::Medium,
        category: "Logging".to_string(),
        title: "Audit logging enabled".to_string(),
        description: "Audit logging should be enabled for compliance and forensics.".to_string(),
        remediation: "Start server with --audit flag.".to_string(),
        passed: target.audit_logging_enabled,
    }
}

fn check_pgwire_auth(target: &AuditTarget) -> SecurityFinding {
    let passed = !target.pgwire_enabled || target.auth_enabled;
    SecurityFinding {
        id: "PROTO-001".to_string(),
        severity: Severity::High,
        category: "Protocol".to_string(),
        title: "PG Wire requires authentication".to_string(),
        description: "PostgreSQL wire protocol should require authentication.".to_string(),
        remediation: "Enable --auth when using --pgwire.".to_string(),
        passed,
    }
}

fn check_mysql_auth(target: &AuditTarget) -> SecurityFinding {
    let passed = !target.mysql_enabled || target.auth_enabled;
    SecurityFinding {
        id: "PROTO-002".to_string(),
        severity: Severity::High,
        category: "Protocol".to_string(),
        title: "MySQL protocol requires authentication".to_string(),
        description: "MySQL wire protocol should require authentication.".to_string(),
        remediation: "Enable --auth when using --mysql.".to_string(),
        passed,
    }
}

/// Format audit result as a human-readable report.
pub fn format_report(result: &SecurityAuditResult) -> String {
    let mut report = String::new();

    report.push_str("╔══════════════════════════════════════════════════════════╗\n");
    report.push_str("║           OntoDB Security Audit Report                  ║\n");
    report.push_str("╚══════════════════════════════════════════════════════════╝\n\n");

    report.push_str(&format!("Timestamp: {}\n", result.timestamp));
    report.push_str(&format!("Score: {}/100\n\n", result.score));

    // Summary by severity
    let critical = result
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical && !f.passed)
        .count();
    let high = result
        .findings
        .iter()
        .filter(|f| f.severity == Severity::High && !f.passed)
        .count();
    let medium = result
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Medium && !f.passed)
        .count();
    let low = result
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Low && !f.passed)
        .count();

    report.push_str("Summary:\n");
    report.push_str(&format!(
        "  Passed: {}/{}\n",
        result.passed, result.total_checks
    ));
    if critical > 0 {
        report.push_str(&format!("  🔴 Critical: {}\n", critical));
    }
    if high > 0 {
        report.push_str(&format!("  🟠 High: {}\n", high));
    }
    if medium > 0 {
        report.push_str(&format!("  🟡 Medium: {}\n", medium));
    }
    if low > 0 {
        report.push_str(&format!("  🟢 Low: {}\n", low));
    }
    report.push('\n');

    // Findings
    report.push_str("Findings:\n");
    for finding in &result.findings {
        let status = if finding.passed {
            "✅ PASS"
        } else {
            "❌ FAIL"
        };
        let severity_icon = match finding.severity {
            Severity::Critical => "🔴",
            Severity::High => "🟠",
            Severity::Medium => "🟡",
            Severity::Low => "🟢",
            Severity::Info => "ℹ️",
        };
        report.push_str(&format!(
            "\n{} [{}] {} {}\n",
            severity_icon, finding.id, status, finding.title
        ));
        report.push_str(&format!("  Category: {}\n", finding.category));
        report.push_str(&format!("  {}\n", finding.description));
        if !finding.passed {
            report.push_str(&format!("  Remediation: {}\n", finding.remediation));
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secure_config() {
        let target = AuditTarget {
            auth_enabled: true,
            api_keys_count: 3,
            tls_enabled: true,
            tls_min_version: "1.2".to_string(),
            rate_limiting_enabled: true,
            audit_logging_enabled: true,
            cors_origins: "https://example.com".to_string(),
            has_default_credentials: false,
            pgwire_enabled: true,
            mysql_enabled: false,
            exposed_ports: vec![7912],
        };

        let result = run_security_audit(&target);
        assert_eq!(result.failed, 0);
        assert_eq!(result.score, 100);
    }

    #[test]
    fn test_insecure_config() {
        let target = AuditTarget {
            auth_enabled: false,
            api_keys_count: 0,
            tls_enabled: false,
            tls_min_version: "1.0".to_string(),
            rate_limiting_enabled: false,
            audit_logging_enabled: false,
            cors_origins: "*".to_string(),
            has_default_credentials: true,
            pgwire_enabled: true,
            mysql_enabled: true,
            exposed_ports: vec![3306, 5432],
        };

        let result = run_security_audit(&target);
        assert!(result.failed > 0);
        assert!(result.score < 50);

        let report = format_report(&result);
        assert!(report.contains("FAIL"));
    }

    #[test]
    fn test_report_format() {
        let target = AuditTarget {
            auth_enabled: true,
            api_keys_count: 1,
            tls_enabled: false,
            tls_min_version: "1.2".to_string(),
            rate_limiting_enabled: true,
            audit_logging_enabled: false,
            cors_origins: "*".to_string(),
            has_default_credentials: false,
            pgwire_enabled: false,
            mysql_enabled: false,
            exposed_ports: vec![],
        };

        let result = run_security_audit(&target);
        let report = format_report(&result);
        assert!(report.contains("OntoDB Security Audit Report"));
        assert!(report.contains("Score:"));
    }
}
