//! Three-Privilege Separation (三权分立) RBAC module for 等保2.0 compliance.
//!
//! Implements mandatory access control with three mutually exclusive admin roles:
//! - **SystemAdmin** (系统管理员): System config, user management, data backup
//! - **SecurityAdmin** (安全管理员): Security policies, access control, key rotation
//! - **AuditAdmin** (审计管理员): Audit log viewing, audit policy config
//!
//! Key constraints (等保2.0 三级):
//! - No single person can hold multiple roles
//! - Audit logs cannot be modified/deleted by any role
//! - System admin cannot view audit log content
//! - Security admin cannot modify system config or view audit logs

use std::collections::HashSet;
use std::sync::Arc;
use parking_lot::RwLock;

/// Admin roles in the three-privilege separation model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AdminRole {
    /// 系统管理员 - System configuration, user management, backup
    SystemAdmin,
    /// 安全管理员 - Security policies, access control, key management
    SecurityAdmin,
    /// 审计管理员 - Audit log viewing, audit policy configuration
    AuditAdmin,
}

impl AdminRole {
    /// Get human-readable name (Chinese).
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::SystemAdmin => "系统管理员",
            Self::SecurityAdmin => "安全管理员",
            Self::AuditAdmin => "审计管理员",
        }
    }

    /// Get English description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::SystemAdmin => "System configuration, user management, data backup",
            Self::SecurityAdmin => "Security policies, access control, key rotation",
            Self::AuditAdmin => "Audit log viewing, audit policy configuration",
        }
    }
}

impl std::fmt::Display for AdminRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// Permissions that can be checked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Permission {
    // === System permissions ===
    /// View system configuration
    ViewSystemConfig,
    /// Modify system configuration
    ModifySystemConfig,
    /// Create/delete users
    ManageUsers,
    /// View user list
    ViewUsers,
    /// Initiate backup
    InitiateBackup,
    /// Restore from backup
    RestoreBackup,

    // === Security permissions ===
    /// View security policies
    ViewSecurityPolicy,
    /// Modify security policies
    ModifySecurityPolicy,
    /// Manage IP whitelist
    ManageIpWhitelist,
    /// Rotate encryption keys
    RotateKeys,
    /// View encryption status
    ViewEncryptionStatus,
    /// Enable/disable encryption
    ToggleEncryption,

    // === Data permissions ===
    /// Read data (SELECT)
    ReadData,
    /// Write data (INSERT/UPDATE/DELETE)
    WriteData,
    /// Modify schema (CREATE/ALTER/DROP)
    ModifySchema,

    // === Audit permissions ===
    /// View audit logs
    ViewAuditLogs,
    /// Export audit logs
    ExportAuditLogs,
    /// Configure audit policies
    ConfigureAuditPolicy,
    /// View audit statistics
    ViewAuditStats,

    // === Cluster permissions ===
    /// View cluster status
    ViewClusterStatus,
    /// Modify cluster configuration
    ModifyClusterConfig,
    /// Add/remove nodes
    ManageClusterNodes,
}

/// RBAC configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RbacConfig {
    /// Enable three-privilege separation.
    pub enabled: bool,
    /// Role assignments: user_id -> role
    pub role_assignments: Vec<RoleAssignment>,
}

/// Role assignment for a user.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoleAssignment {
    /// User identifier (API key prefix or username)
    pub user_id: String,
    /// Assigned role
    pub role: AdminRole,
    /// When the role was assigned
    pub assigned_at: String,
    /// Who assigned this role
    pub assigned_by: Option<String>,
}

impl Default for RbacConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            role_assignments: Vec::new(),
        }
    }
}

/// RBAC manager — enforces three-privilege separation.
#[derive(Clone)]
pub struct RbacManager {
    config: Arc<RwLock<RbacConfig>>,
    /// Role assignments: user_id -> role
    assignments: Arc<RwLock<std::collections::HashMap<String, AdminRole>>>,
}

impl RbacManager {
    /// Create a new RBAC manager.
    pub fn new(config: RbacConfig) -> Self {
        let mut assignments = std::collections::HashMap::new();
        for assignment in &config.role_assignments {
            assignments.insert(assignment.user_id.clone(), assignment.role);
        }

        Self {
            config: Arc::new(RwLock::new(config)),
            assignments: Arc::new(RwLock::new(assignments)),
        }
    }

    /// Check if RBAC is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.read().enabled
    }

    /// Get the role for a user.
    pub fn get_role(&self, user_id: &str) -> Option<AdminRole> {
        self.assignments.read().get(user_id).copied()
    }

    /// Assign a role to a user.
    ///
    /// Returns error if the user already has a different role (one user, one role).
    pub fn assign_role(&self, user_id: &str, role: AdminRole, assigned_by: Option<&str>) -> Result<(), RbacError> {
        let mut assignments = self.assignments.write();

        // Check if user already has a role
        if let Some(existing) = assignments.get(user_id) {
            if *existing != role {
                return Err(RbacError::RoleConflict {
                    user_id: user_id.to_string(),
                    existing_role: *existing,
                    new_role: role,
                });
            }
            return Ok(()); // Same role, no-op
        }

        // Check if the new role is already assigned to someone else
        // (optional: enforce unique role holders)
        assignments.insert(user_id.to_string(), role);

        // Update config
        let mut config = self.config.write();
        config.role_assignments.push(RoleAssignment {
            user_id: user_id.to_string(),
            role,
            assigned_at: chrono::Utc::now().to_rfc3339(),
            assigned_by: assigned_by.map(|s| s.to_string()),
        });

        tracing::info!(
            "Role assigned: {} -> {} (by {:?})",
            user_id,
            role.display_name(),
            assigned_by
        );

        Ok(())
    }

    /// Remove a role from a user.
    pub fn remove_role(&self, user_id: &str) -> bool {
        let removed = self.assignments.write().remove(user_id).is_some();
        if removed {
            let mut config = self.config.write();
            config.role_assignments.retain(|a| a.user_id != user_id);
            tracing::info!("Role removed: {}", user_id);
        }
        removed
    }

    /// Check if a user has a specific permission.
    pub fn has_permission(&self, user_id: &str, permission: &Permission) -> bool {
        let assignments = self.assignments.read();
        match assignments.get(user_id) {
            Some(role) => Self::role_has_permission(*role, permission),
            None => false, // No role = no permission
        }
    }

    /// Check if a role has a specific permission.
    pub fn role_has_permission(role: AdminRole, permission: &Permission) -> bool {
        match role {
            AdminRole::SystemAdmin => Self::system_admin_permissions().contains(permission),
            AdminRole::SecurityAdmin => Self::security_admin_permissions().contains(permission),
            AdminRole::AuditAdmin => Self::audit_admin_permissions().contains(permission),
        }
    }

    /// Get all permissions for SystemAdmin.
    pub fn system_admin_permissions() -> HashSet<Permission> {
        let mut perms = HashSet::new();
        // System management
        perms.insert(Permission::ViewSystemConfig);
        perms.insert(Permission::ModifySystemConfig);
        perms.insert(Permission::ManageUsers);
        perms.insert(Permission::ViewUsers);
        perms.insert(Permission::InitiateBackup);
        perms.insert(Permission::RestoreBackup);
        // Data access
        perms.insert(Permission::ReadData);
        perms.insert(Permission::WriteData);
        perms.insert(Permission::ModifySchema);
        // Cluster
        perms.insert(Permission::ViewClusterStatus);
        perms.insert(Permission::ModifyClusterConfig);
        perms.insert(Permission::ManageClusterNodes);
        // View-only for security and audit
        perms.insert(Permission::ViewSecurityPolicy);
        perms.insert(Permission::ViewEncryptionStatus);
        perms
    }

    /// Get all permissions for SecurityAdmin.
    pub fn security_admin_permissions() -> HashSet<Permission> {
        let mut perms = HashSet::new();
        // Security management
        perms.insert(Permission::ViewSecurityPolicy);
        perms.insert(Permission::ModifySecurityPolicy);
        perms.insert(Permission::ManageIpWhitelist);
        perms.insert(Permission::RotateKeys);
        perms.insert(Permission::ViewEncryptionStatus);
        perms.insert(Permission::ToggleEncryption);
        // Data read-only
        perms.insert(Permission::ReadData);
        // Cluster view
        perms.insert(Permission::ViewClusterStatus);
        // User view
        perms.insert(Permission::ViewUsers);
        // System config view (but not modify)
        perms.insert(Permission::ViewSystemConfig);
        perms
    }

    /// Get all permissions for AuditAdmin.
    pub fn audit_admin_permissions() -> HashSet<Permission> {
        let mut perms = HashSet::new();
        // Audit management (highest privilege for audit)
        perms.insert(Permission::ViewAuditLogs);
        perms.insert(Permission::ExportAuditLogs);
        perms.insert(Permission::ConfigureAuditPolicy);
        perms.insert(Permission::ViewAuditStats);
        // View-only for system and security
        perms.insert(Permission::ViewSystemConfig);
        perms.insert(Permission::ViewSecurityPolicy);
        perms.insert(Permission::ViewUsers);
        perms.insert(Permission::ViewClusterStatus);
        perms.insert(Permission::ViewEncryptionStatus);
        perms
    }

    /// Get all role assignments.
    pub fn list_assignments(&self) -> Vec<RoleAssignment> {
        self.config.read().role_assignments.clone()
    }

    /// Get status summary.
    pub fn status(&self) -> RbacStatus {
        let config = self.config.read();
        RbacStatus {
            enabled: config.enabled,
            system_admin_count: self.count_role(AdminRole::SystemAdmin),
            security_admin_count: self.count_role(AdminRole::SecurityAdmin),
            audit_admin_count: self.count_role(AdminRole::AuditAdmin),
        }
    }

    /// Count users with a specific role.
    fn count_role(&self, role: AdminRole) -> usize {
        self.assignments.read().values().filter(|r| **r == role).count()
    }
}

/// RBAC status for monitoring.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RbacStatus {
    pub enabled: bool,
    pub system_admin_count: usize,
    pub security_admin_count: usize,
    pub audit_admin_count: usize,
}

/// RBAC errors.
#[derive(Debug, thiserror::Error)]
pub enum RbacError {
    #[error("Role conflict: user {user_id} already has role {existing_role}, cannot assign {new_role}")]
    RoleConflict {
        user_id: String,
        existing_role: AdminRole,
        new_role: AdminRole,
    },

    #[error("Permission denied: {reason}")]
    PermissionDenied { reason: String },

    #[error("RBAC not enabled")]
    NotEnabled,
}

/// Macro for checking permissions and returning error if denied.
#[macro_export]
macro_rules! require_permission {
    ($rbac:expr, $user_id:expr, $permission:expr) => {
        if !$rbac.is_enabled() {
            return Err($crate::rbac::RbacError::NotEnabled);
        }
        if !$rbac.has_permission($user_id, &$permission) {
            return Err($crate::rbac::RbacError::PermissionDenied {
                reason: format!(
                    "User {} does not have permission {:?}",
                    $user_id, $permission
                ),
            });
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_manager() -> RbacManager {
        let config = RbacConfig {
            enabled: true,
            role_assignments: Vec::new(),
        };
        RbacManager::new(config)
    }

    #[test]
    fn test_role_assignment() {
        let manager = create_test_manager();

        // Assign roles
        manager.assign_role("sys_admin_1", AdminRole::SystemAdmin, None).unwrap();
        manager.assign_role("sec_admin_1", AdminRole::SecurityAdmin, None).unwrap();
        manager.assign_role("aud_admin_1", AdminRole::AuditAdmin, None).unwrap();

        // Verify roles
        assert_eq!(manager.get_role("sys_admin_1"), Some(AdminRole::SystemAdmin));
        assert_eq!(manager.get_role("sec_admin_1"), Some(AdminRole::SecurityAdmin));
        assert_eq!(manager.get_role("aud_admin_1"), Some(AdminRole::AuditAdmin));
    }

    #[test]
    fn test_role_conflict() {
        let manager = create_test_manager();

        manager.assign_role("user1", AdminRole::SystemAdmin, None).unwrap();

        // Cannot assign different role to same user
        let result = manager.assign_role("user1", AdminRole::SecurityAdmin, None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RbacError::RoleConflict { .. }));
    }

    #[test]
    fn test_system_admin_permissions() {
        let manager = create_test_manager();
        manager.assign_role("sys", AdminRole::SystemAdmin, None).unwrap();

        // System admin can manage system
        assert!(manager.has_permission("sys", &Permission::ModifySystemConfig));
        assert!(manager.has_permission("sys", &Permission::ManageUsers));
        assert!(manager.has_permission("sys", &Permission::InitiateBackup));

        // System admin can read/write data
        assert!(manager.has_permission("sys", &Permission::ReadData));
        assert!(manager.has_permission("sys", &Permission::WriteData));

        // System admin CANNOT view audit logs
        assert!(!manager.has_permission("sys", &Permission::ViewAuditLogs));
        assert!(!manager.has_permission("sys", &Permission::ConfigureAuditPolicy));

        // System admin CANNOT modify security policy
        assert!(!manager.has_permission("sys", &Permission::ModifySecurityPolicy));
        assert!(!manager.has_permission("sys", &Permission::RotateKeys));
    }

    #[test]
    fn test_security_admin_permissions() {
        let manager = create_test_manager();
        manager.assign_role("sec", AdminRole::SecurityAdmin, None).unwrap();

        // Security admin can manage security
        assert!(manager.has_permission("sec", &Permission::ModifySecurityPolicy));
        assert!(manager.has_permission("sec", &Permission::ManageIpWhitelist));
        assert!(manager.has_permission("sec", &Permission::RotateKeys));

        // Security admin can read data
        assert!(manager.has_permission("sec", &Permission::ReadData));

        // Security admin CANNOT write data
        assert!(!manager.has_permission("sec", &Permission::WriteData));
        assert!(!manager.has_permission("sec", &Permission::ModifySchema));

        // Security admin CANNOT view audit logs
        assert!(!manager.has_permission("sec", &Permission::ViewAuditLogs));

        // Security admin CANNOT modify system config
        assert!(!manager.has_permission("sec", &Permission::ModifySystemConfig));
        assert!(!manager.has_permission("sec", &Permission::ManageUsers));
    }

    #[test]
    fn test_audit_admin_permissions() {
        let manager = create_test_manager();
        manager.assign_role("aud", AdminRole::AuditAdmin, None).unwrap();

        // Audit admin can view and configure audit
        assert!(manager.has_permission("aud", &Permission::ViewAuditLogs));
        assert!(manager.has_permission("aud", &Permission::ExportAuditLogs));
        assert!(manager.has_permission("aud", &Permission::ConfigureAuditPolicy));

        // Audit admin can view (but not modify) system and security
        assert!(manager.has_permission("aud", &Permission::ViewSystemConfig));
        assert!(manager.has_permission("aud", &Permission::ViewSecurityPolicy));

        // Audit admin CANNOT modify anything
        assert!(!manager.has_permission("aud", &Permission::ModifySystemConfig));
        assert!(!manager.has_permission("aud", &Permission::ModifySecurityPolicy));
        assert!(!manager.has_permission("aud", &Permission::WriteData));
        assert!(!manager.has_permission("aud", &Permission::ManageUsers));
    }

    #[test]
    fn test_no_role_no_permission() {
        let manager = create_test_manager();

        // User without role has no permissions
        assert!(!manager.has_permission("unknown", &Permission::ReadData));
        assert!(!manager.has_permission("unknown", &Permission::ViewAuditLogs));
    }

    #[test]
    fn test_remove_role() {
        let manager = create_test_manager();
        manager.assign_role("user1", AdminRole::SystemAdmin, None).unwrap();

        assert!(manager.remove_role("user1"));
        assert_eq!(manager.get_role("user1"), None);
        assert!(!manager.has_permission("user1", &Permission::ReadData));
    }

    #[test]
    fn test_mutual_exclusivity() {
        // Verify that no permission overlaps between roles in a way that
        // would allow one person to bypass checks

        let sys_perms = RbacManager::system_admin_permissions();
        let sec_perms = RbacManager::security_admin_permissions();
        let aud_perms = RbacManager::audit_admin_permissions();

        // Audit permissions should NOT be in system or security admin
        assert!(!sys_perms.contains(&Permission::ViewAuditLogs));
        assert!(!sec_perms.contains(&Permission::ViewAuditLogs));
        assert!(!sys_perms.contains(&Permission::ConfigureAuditPolicy));
        assert!(!sec_perms.contains(&Permission::ConfigureAuditPolicy));

        // Security modification should NOT be in system admin
        assert!(!sys_perms.contains(&Permission::ModifySecurityPolicy));
        assert!(!sys_perms.contains(&Permission::RotateKeys));

        // System modification should NOT be in security admin
        assert!(!sec_perms.contains(&Permission::ModifySystemConfig));
        assert!(!sec_perms.contains(&Permission::ManageUsers));

        // Data write should NOT be in security or audit admin
        assert!(!sec_perms.contains(&Permission::WriteData));
        assert!(!aud_perms.contains(&Permission::WriteData));
    }

    #[test]
    fn test_status() {
        let manager = create_test_manager();
        manager.assign_role("sys1", AdminRole::SystemAdmin, None).unwrap();
        manager.assign_role("sys2", AdminRole::SystemAdmin, None).unwrap();
        manager.assign_role("sec1", AdminRole::SecurityAdmin, None).unwrap();
        manager.assign_role("aud1", AdminRole::AuditAdmin, None).unwrap();

        let status = manager.status();
        assert!(status.enabled);
        assert_eq!(status.system_admin_count, 2);
        assert_eq!(status.security_admin_count, 1);
        assert_eq!(status.audit_admin_count, 1);
    }
}
