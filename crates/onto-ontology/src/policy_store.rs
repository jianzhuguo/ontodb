// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! Policy store: engine-backed persistence for the RBAC policy state.
//!
//! Design: `docs/RBAC权限模型详细设计.md` §9.1 — every grant/principal change
//! is one WAL-protected write (`__policy__state` row, JSON snapshot). The
//! in-memory [`PolicyEngine`] is the PDP; this store makes its state durable
//! and reloads it at boot. Row shape is a full snapshot: policy state is
//! small (KBs), and a single-row rewrite keeps updates atomic.

use onto_core::policy::{CredentialRecord, Grant, PolicyEngine, PolicySnapshot, Principal};
use onto_core::{CoreError, Result};
use onto_storage::LsmEngine;
use std::sync::Arc;

/// Storage key for the policy snapshot (control-plane row, like `__ns__`).
const POLICY_KEY: &[u8] = b"__policy__state";

/// Engine-backed persistence for [`PolicySnapshot`]s.
pub struct PolicyStore {
    engine: Arc<LsmEngine>,
}

impl PolicyStore {
    pub fn new(engine: Arc<LsmEngine>) -> Self {
        Self { engine }
    }

    /// Load the persisted snapshot; a missing row means a fresh install.
    pub fn load(&self) -> Result<PolicySnapshot> {
        match self.engine.get(POLICY_KEY)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| CoreError::Serialization(format!("policy snapshot: {e}"))),
            None => Ok(PolicySnapshot::default()),
        }
    }

    pub fn save(&self, snap: &PolicySnapshot) -> Result<()> {
        let bytes = serde_json::to_vec(snap)
            .map_err(|e| CoreError::Serialization(format!("policy snapshot: {e}")))?;
        self.engine.put(POLICY_KEY.to_vec(), bytes)
    }
}

/// Durable policy service: the PDP plus its store, updated together so a
/// granted permission survives a restart (design §9.1).
pub struct PolicyService {
    pub engine: Arc<PolicyEngine>,
    store: PolicyStore,
}

impl PolicyService {
    /// Open the service, loading any persisted state (fresh = empty).
    pub fn open(store: PolicyStore) -> Result<Arc<Self>> {
        let engine = Arc::new(PolicyEngine::new());
        engine.import_snapshot(store.load()?);
        Ok(Arc::new(Self { engine, store }))
    }

    fn persist(&self) -> Result<()> {
        self.store.save(&self.engine.export_snapshot())
    }

    // -- principals & credentials --

    pub fn upsert_principal(&self, p: Principal) -> Result<()> {
        self.engine.upsert_principal(p);
        self.persist()
    }

    pub fn remove_principal(&self, id: &str) -> Result<bool> {
        let removed = self.engine.remove_principal(id);
        self.persist()?;
        Ok(removed)
    }

    pub fn put_credential(&self, rec: CredentialRecord) -> Result<()> {
        self.engine.put_credential(rec);
        self.persist()
    }

    /// Register a bearer token (only its hash is stored).
    pub fn put_token(&self, token: &str, principal_id: &str) -> Result<()> {
        self.engine.put_token(token, principal_id);
        self.persist()
    }

    // -- roles & grants --

    pub fn grant_role(&self, principal_id: &str, role: &str) -> Result<bool> {
        let ok = self.engine.grant_role(principal_id, role);
        self.persist()?;
        Ok(ok)
    }

    pub fn revoke_role(&self, principal_id: &str, role: &str) -> Result<bool> {
        let ok = self.engine.revoke_role(principal_id, role);
        self.persist()?;
        Ok(ok)
    }

    pub fn grant(&self, g: Grant) -> Result<()> {
        self.engine.grant(g);
        self.persist()
    }

    pub fn revoke(&self, g: &Grant) -> Result<bool> {
        let removed = self.engine.revoke(g);
        self.persist()?;
        Ok(removed)
    }

    /// DROP ROLE: name is gone from every principal and its grants vanish.
    pub fn drop_role(&self, role: &str) -> Result<()> {
        self.engine.drop_role(role);
        self.persist()
    }

    // -- reads (no persistence) --

    pub fn check_class(&self, principal_id: &str, action: onto_core::policy::Action, ns: &str, class: &str) -> bool {
        self.engine.check_class(principal_id, action, ns, class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onto_core::policy::{Action, ObjectRef, PrincipalKind, CRED_SALT_LEN, CRED_SCHEME_BLAKE3};
    use onto_storage::StorageOptions;
    use tempfile::tempdir;

    fn open_store(dir: &std::path::Path) -> PolicyStore {
        let engine = LsmEngine::open(StorageOptions {
            data_dir: dir.to_path_buf(),
            ..Default::default()
        })
        .unwrap();
        PolicyStore::new(Arc::new(engine))
    }

    #[test]
    fn policy_survives_reopen() {
        let dir = tempdir().unwrap();
        {
            let svc = PolicyService::open(open_store(dir.path())).unwrap();
            svc.upsert_principal(Principal {
                id: "alice".into(),
                kind: PrincipalKind::User,
                roles: ["analyst".to_string()].into(),
                token_epoch: 0,
                disabled: false,
                cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
            })
            .unwrap();
            svc.grant(Grant {
                role: "analyst".into(),
                action: Action::Read,
                object: ObjectRef::Namespace("erp".into()),
            })
            .unwrap();
            svc.put_token("tok-a", "alice").unwrap();
        }
        // Fresh service over the same engine dir: grants and tokens survive.
        let svc = PolicyService::open(open_store(dir.path())).unwrap();
        assert!(svc.check_class("alice", Action::Read, "erp", "orders"));
        assert!(svc
            .engine
            .authenticate_token("tok-a", 0)
            .is_some());
        assert_eq!(svc.engine.grants().len(), 1);
    }

    #[test]
    fn revoke_is_durable() {
        let dir = tempdir().unwrap();
        let svc = PolicyService::open(open_store(dir.path())).unwrap();
        svc.upsert_principal(Principal {
            id: "alice".into(),
            kind: PrincipalKind::User,
            roles: ["r".to_string()].into(),
            token_epoch: 0,
            disabled: false,
            cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
        })
        .unwrap();
        let g = Grant {
            role: "r".into(),
            action: Action::Write,
            object: ObjectRef::Class {
                ns: "erp".into(),
                class: "orders".into(),
            },
        };
        svc.grant(g.clone()).unwrap();
        assert!(svc.check_class("alice", Action::Write, "erp", "orders"));
        assert!(svc.revoke(&g).unwrap());

        let svc = PolicyService::open(open_store(dir.path())).unwrap();
        assert!(!svc.check_class("alice", Action::Write, "erp", "orders"));
    }

    #[test]
    fn credential_roundtrip_via_store() {
        let dir = tempdir().unwrap();
        {
            let svc = PolicyService::open(open_store(dir.path())).unwrap();
            svc.upsert_principal(Principal {
                id: "bob".into(),
                kind: PrincipalKind::User,
                roles: Default::default(),
                token_epoch: 0,
                disabled: false,
                cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
            })
            .unwrap();
            let salt = [9u8; CRED_SALT_LEN];
            let hash = onto_core::policy::password_hash("pw", &salt, None);
            svc.put_credential(CredentialRecord {
                principal_id: "bob".into(),
                salt,
                hash,
                cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
            })
            .unwrap();
        }
        // Engine dropped (memtable flushed) before reopening on the same dir.
        let svc = PolicyService::open(open_store(dir.path())).unwrap();
        assert!(svc.engine.authenticate_password("bob", "pw", None).is_some());
        assert!(svc.engine.authenticate_password("bob", "bad", None).is_none());
    }
}
