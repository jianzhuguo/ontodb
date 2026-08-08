//! OntoDB Enterprise features.
//!
//! This crate contains proprietary enterprise functionality:
//! - Cluster management (Raft-based multi-replica)
//! - Data sharding
//! - Enterprise security (LDAP/SAML, audit logs, encryption)
//! - Backup & recovery (incremental, PITR, offsite)
//! - Observability (advanced monitoring, slow query analysis)
//!
//! Feature flags control which modules are compiled:
//! - `cluster` — Raft consensus, multi-replica, automatic failover
//! - `sharding` — data sharding, cross-shard queries
//! - `security` — LDAP/SAML, audit logging, data encryption
//! - `backup` — incremental backup, PITR, offsite backup
//! - `observability` — advanced monitoring, slow query analysis, auto-tuning

#[cfg(feature = "cluster")]
pub mod cluster;

#[cfg(feature = "sharding")]
pub mod sharding;

#[cfg(feature = "security")]
pub mod security;

#[cfg(feature = "backup")]
pub mod backup;

#[cfg(feature = "observability")]
pub mod observability;

/// Enterprise license verification placeholder.
pub fn is_enterprise_enabled() -> bool {
    // TODO: implement license verification
    false
}
