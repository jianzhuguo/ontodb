// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! RBAC M1 api_keys migration (docs/RBAC权限模型详细设计.md §9.2).
//!
//! Maps `api_keys.json` into the policy store at startup:
//! - the three permission tiers become the built-in roles
//!   `_readonly`/`_readwrite`/`_admin` (granted `NAMESPACE *` per level);
//! - every key becomes a ServiceAccount principal in `__sys__.users` whose
//!   bearer token is the key itself (only `BLAKE3(key)` is stored).
//!
//! Idempotent and config-as-code: the file stays read-only and is re-mapped
//! at every start ("迁移一次性、幂等"). Keys without an explicit `principal`
//! field get a stable derived id (`key_<8 hex of BLAKE3(key)>`) so legacy
//! configs work unchanged (M1 acceptance ③). `SYSTEM` object grants are not
//! modeled in M1 (namespace/class only).

use crate::auth::{ApiKeyConfig, AuthConfig, Permission};
use onto_core::policy::{
    token_hash, Action, Grant, ObjectRef, Principal, PrincipalKind, BUILTIN_ROLE_ADMIN,
    BUILTIN_ROLE_READONLY, BUILTIN_ROLE_READWRITE, CRED_SCHEME_BLAKE3,
};
use onto_core::Result;
use onto_ontology::policy_store::PolicyService;
use std::collections::HashMap;

/// Map `api_keys.json` into the policy store. Returns what the batch did
/// (design §12 「一批=一审计记录」: the whole migration run is one audit
/// record). Safe to re-run: ensure-style, converges to the config's content.
pub fn migrate_api_keys(config: &AuthConfig, svc: &PolicyService) -> Result<MigrationOutcome> {
    let roles_created = ensure_builtin_roles(svc)?;

    // Two passes: keys sharing a principal union their tier (the model unions
    // roles), so reconcile against the strongest tier seen for that id.
    let mut tier: HashMap<String, &'static str> = HashMap::new();
    for key in &config.keys {
        let id = principal_id_for(key);
        let want = tier_role(&key.permission);
        let slot = tier.entry(id).or_insert(want);
        if tier_rank(want) > tier_rank(slot) {
            *slot = want;
        }
    }

    let mut principals_created = 0usize;
    for key in &config.keys {
        let id = principal_id_for(key);
        if svc.engine.principal(&id).is_none() {
            svc.upsert_principal(Principal {
                id: id.clone(),
                kind: PrincipalKind::ServiceAccount,
                roles: Default::default(),
                token_epoch: 0,
                disabled: false,
                cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
            })?;
            principals_created += 1;
        }
        // Config-managed keys are not epoch-gated (lifetime = config file).
        svc.put_token(&key.key, &id, None)?;
    }

    for (id, want) in &tier {
        reconcile_tier(svc, id, want)?;
    }
    Ok(MigrationOutcome {
        keys_mapped: config.keys.len(),
        principals_created,
        roles_created,
    })
}

/// What a migration batch did (for the one audit record and the startup log).
pub struct MigrationOutcome {
    pub keys_mapped: usize,
    pub principals_created: usize,
    pub roles_created: usize,
}

impl MigrationOutcome {
    /// Secret-free audit/log detail for the batch (never names key material).
    pub fn audit_detail(&self) -> String {
        format!(
            "api_keys migration: {} key(s) mapped to principals ({} created); \
             built-in roles _readonly/_readwrite/_admin ensured ({} created)",
            self.keys_mapped, self.principals_created, self.roles_created
        )
    }
}

/// The three built-in tiers (design §9.2), each granted `NAMESPACE *` at its
/// action level. Idempotent: roles may already exist and grants deduplicate.
/// Returns how many roles were newly created.
fn ensure_builtin_roles(svc: &PolicyService) -> Result<usize> {
    let mut created = 0usize;
    // INFER on every tier: before T4.3a `required_action(Infer{})` mapped to
    // Read, so migrated api_keys users could run INFER statements at every
    // level. Actions match exactly in the PDP, so dropping INFER here would
    // silently strip that capability on migration.
    for (role, actions) in [
        (BUILTIN_ROLE_READONLY, vec![Action::Read, Action::Infer]),
        (
            BUILTIN_ROLE_READWRITE,
            vec![Action::Read, Action::Infer, Action::Write],
        ),
        (
            BUILTIN_ROLE_ADMIN,
            vec![Action::Read, Action::Infer, Action::Write, Action::Ddl],
        ),
    ] {
        if svc.create_role(role)? {
            created += 1;
        }
        for action in actions {
            svc.grant(Grant {
                role: role.to_string(),
                action,
                object: ObjectRef::Namespace("*".to_string()),
                options: Default::default(),
            })?;
        }
    }
    Ok(created)
}

/// Keep only `want` of the three built-in tier roles on `id`. Any other role
/// membership is the operator's own DDL and is left untouched.
fn reconcile_tier(svc: &PolicyService, id: &str, want: &str) -> Result<()> {
    for role in [
        BUILTIN_ROLE_READONLY,
        BUILTIN_ROLE_READWRITE,
        BUILTIN_ROLE_ADMIN,
    ] {
        let has = svc
            .engine
            .principal(id)
            .is_some_and(|p| p.roles.contains(role));
        if role == want {
            if !has {
                svc.grant_role(id, role)?;
            }
        } else if has {
            svc.revoke_role(id, role)?;
        }
    }
    Ok(())
}

fn tier_role(p: &Permission) -> &'static str {
    match p {
        Permission::ReadOnly => BUILTIN_ROLE_READONLY,
        Permission::ReadWrite => BUILTIN_ROLE_READWRITE,
        Permission::Admin => BUILTIN_ROLE_ADMIN,
    }
}

fn tier_rank(role: &str) -> u8 {
    match role {
        BUILTIN_ROLE_READONLY => 1,
        BUILTIN_ROLE_READWRITE => 2,
        _ => 3,
    }
}

/// Principal id for a key: the explicit `principal` config field when set,
/// otherwise a stable derived id (`key_<8 hex of BLAKE3(key)>`) that leaks no
/// key material into audit records.
fn principal_id_for(key: &ApiKeyConfig) -> String {
    match key.principal.as_deref() {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            let h = token_hash(&key.key);
            let mut id = String::from("key_");
            for b in &h[..4] {
                use std::fmt::Write as _;
                let _ = write!(id, "{b:02x}");
            }
            id
        }
    }
}

/// Bootstrap admin principal id (design §11).
pub const BOOTSTRAP_ADMIN_ID: &str = "admin";

/// What a bootstrap run did (design §11 Bootstrap). The minted token is
/// returned only to the caller for the one-time stdout print — it never
/// reaches logs or audit records.
pub struct BootstrapOutcome {
    /// Secret-free detail for the startup log / one `grant_change` record.
    pub audit_detail: String,
    /// Freshly minted token to print exactly once (`None` = nothing to print:
    /// injected tokens are operator-known, later starts are silent).
    pub minted_token: Option<String>,
}

/// Bootstrap the admin identity (design §11: 「首次启动生成 admin token 打到
/// stdout 一次（或 `--bootstrap-admin-token` 注入）」).
///
/// - **First run** (policy store holds no principal at all): create `admin`
///   in the built-in `_admin` tier and mint a 256-bit token. The token is
///   epoch-gated — `ALTER USER ... PASSWORD` / `ROTATE TOKEN` kill it like
///   any session credential.
/// - **Injected** (`--bootstrap-admin-token`): ensure-style like the
///   api_keys migration — the admin principal is created when missing and
///   the token is registered unbound (config-managed lifetime, survives
///   epoch bumps, re-asserted at every start).
/// - Any later start without the flag is a no-op; the token is never
///   re-printed.
pub fn bootstrap_admin(svc: &PolicyService, injected_token: Option<&str>) -> Result<BootstrapOutcome> {
    let first_run = svc.engine.principals().is_empty();
    let token = match injected_token {
        Some(t) if !t.trim().is_empty() => Some((t.to_string(), false)),
        _ if first_run => {
            let mut raw = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut raw);
            let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
            Some((token, true))
        }
        _ => None,
    };
    if token.is_none() && !first_run {
        return Ok(BootstrapOutcome {
            audit_detail: String::new(),
            minted_token: None,
        });
    }

    let mut principal_created = false;
    if svc.engine.principal(BOOTSTRAP_ADMIN_ID).is_none() {
        svc.upsert_principal(Principal {
            id: BOOTSTRAP_ADMIN_ID.to_string(),
            kind: PrincipalKind::User,
            roles: std::iter::once(BUILTIN_ROLE_ADMIN.to_string()).collect(),
            token_epoch: 0,
            disabled: false,
            cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
        })?;
        principal_created = true;
    }

    let (tok, minted) = token.expect("token decided above");
    if minted {
        // Session-style credential: dies with the principal's epoch.
        let epoch = svc
            .engine
            .principal(BOOTSTRAP_ADMIN_ID)
            .map(|p| p.token_epoch)
            .unwrap_or(0);
        svc.put_token(&tok, BOOTSTRAP_ADMIN_ID, Some(epoch))?;
    } else {
        // Operator-managed secret: config-key semantics (design §9.2) —
        // lifetime follows the flag/env, not the token epoch.
        svc.put_token(&tok, BOOTSTRAP_ADMIN_ID, None)?;
    }

    let audit_detail = format!(
        "bootstrap: admin principal '{}'{}; token {}",
        BOOTSTRAP_ADMIN_ID,
        if principal_created {
            " created in tier _admin"
        } else {
            " ensured"
        },
        if minted {
            "minted (printed once to stdout)"
        } else {
            "registered (operator-injected)"
        }
    );
    Ok(BootstrapOutcome {
        audit_detail,
        minted_token: minted.then_some(tok),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onto_core::policy::PolicyEngine;
    use onto_ontology::policy_store::PolicyStore;
    use onto_storage::{LsmEngine, StorageOptions};
    use std::sync::Arc;

    fn key(secret: &str, permission: Permission, principal: Option<&str>) -> ApiKeyConfig {
        ApiKeyConfig {
            key: secret.to_string(),
            description: format!("key {secret}"),
            permission,
            rate_limit: None,
            allowed_ips: None,
            namespace: None,
            principal: principal.map(String::from),
        }
    }

    fn config(keys: Vec<ApiKeyConfig>) -> AuthConfig {
        AuthConfig {
            enabled: true,
            keys,
            default_permission: Permission::ReadOnly,
        }
    }

    fn service(dir: &std::path::Path) -> Arc<PolicyService> {
        let engine = Arc::new(
            LsmEngine::open(StorageOptions {
                data_dir: dir.to_path_buf(),
                ..Default::default()
            })
            .unwrap(),
        );
        PolicyService::open(PolicyStore::new(engine)).unwrap()
    }

    #[test]
    fn migration_maps_tiers_tokens_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let cfg = config(vec![
            key("sec-ro", Permission::ReadOnly, None),
            key("sec-rw", Permission::ReadWrite, None),
            key("sec-ad", Permission::Admin, None),
        ]);
        assert_eq!(migrate_api_keys(&cfg, &svc).unwrap().keys_mapped, 3);

        // Built-in roles exist with the tier grants on NAMESPACE *.
        let eng: &PolicyEngine = &svc.engine;
        for role in [BUILTIN_ROLE_READONLY, BUILTIN_ROLE_READWRITE, BUILTIN_ROLE_ADMIN] {
            assert!(eng.role_exists(role), "role {role} must exist");
        }
        assert_eq!(eng.grants_for_role(BUILTIN_ROLE_READONLY).len(), 2);
        assert_eq!(eng.grants_for_role(BUILTIN_ROLE_READWRITE).len(), 3);
        assert_eq!(eng.grants_for_role(BUILTIN_ROLE_ADMIN).len(), 4);

        // Each key resolves to its own ServiceAccount with the right tier.
        let ro = eng.authenticate_token("sec-ro").unwrap();
        assert!(matches!(ro.kind, PrincipalKind::ServiceAccount));
        assert!(ro.roles.contains(BUILTIN_ROLE_READONLY));
        assert!(!ro.roles.contains(BUILTIN_ROLE_READWRITE));
        let rw = eng.authenticate_token("sec-rw").unwrap();
        assert!(rw.roles.contains(BUILTIN_ROLE_READWRITE));
        let ad = eng.authenticate_token("sec-ad").unwrap();
        assert!(ad.roles.contains(BUILTIN_ROLE_ADMIN));

        // Tier behavior matrix (design §9.2): no ride-along between levels.
        // INFER rides on every tier (see `ensure_builtin_roles`).
        let (ns, cls) = ("any", "thing");
        assert!(eng.check_class(&ro.id, Action::Read, ns, cls));
        assert!(eng.check_class(&ro.id, Action::Infer, ns, cls));
        assert!(!eng.check_class(&ro.id, Action::Write, ns, cls));
        assert!(!eng.check_class(&ro.id, Action::Ddl, ns, cls));
        assert!(eng.check_class(&rw.id, Action::Read, ns, cls));
        assert!(eng.check_class(&rw.id, Action::Infer, ns, cls));
        assert!(eng.check_class(&rw.id, Action::Write, ns, cls));
        assert!(!eng.check_class(&rw.id, Action::Ddl, ns, cls));
        assert!(eng.check_class(&ad.id, Action::Infer, ns, cls));
        assert!(eng.check_class(&ad.id, Action::Ddl, ns, cls));

        // Unknown keys resolve to nothing.
        assert!(eng.authenticate_token("nope").is_none());
    }

    #[test]
    fn migration_is_idempotent_and_preserves_extra_roles() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let cfg = config(vec![key("sec-1", Permission::ReadWrite, Some("alice"))]);
        let first = migrate_api_keys(&cfg, &svc).unwrap();
        assert_eq!(first.principals_created, 1);
        assert_eq!(first.roles_created, 3);
        svc.grant_role("alice", "analyst").unwrap();
        let grants_before = svc.engine.grants_for_role(BUILTIN_ROLE_READWRITE).len();

        // Second run converges without clobbering operator-added state, and
        // the batch reports no creation (audit sees a no-op batch).
        let second = migrate_api_keys(&cfg, &svc).unwrap();
        assert_eq!(second.principals_created, 0);
        assert_eq!(second.roles_created, 0);
        let alice = svc.engine.principal("alice").unwrap();
        assert!(alice.roles.contains("analyst"), "extra role must survive");
        assert!(alice.roles.contains(BUILTIN_ROLE_READWRITE));
        assert!(!alice.roles.contains(BUILTIN_ROLE_READONLY));
        assert_eq!(
            svc.engine.grants_for_role(BUILTIN_ROLE_READWRITE).len(),
            grants_before,
            "grants must not duplicate"
        );
        assert_eq!(
            svc.engine.authenticate_token("sec-1").unwrap().id,
            "alice"
        );
    }

    #[test]
    fn migration_audit_detail_names_counts_but_no_key_material() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let cfg = config(vec![
            key("sec-1", Permission::ReadOnly, None),
            key("sec-2", Permission::Admin, Some("alice")),
        ]);
        let outcome = migrate_api_keys(&cfg, &svc).unwrap();
        let detail = outcome.audit_detail();
        assert!(detail.contains("2 key(s)"), "{detail}");
        assert!(detail.contains("_readonly/_readwrite/_admin"), "{detail}");
        // Secrets must never reach the audit trail.
        assert!(!detail.contains("sec-1"), "{detail}");
        assert!(!detail.contains("sec-2"), "{detail}");
    }

    #[test]
    fn explicit_principal_wins_and_derived_id_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let cfg = config(vec![
            key("sec-a", Permission::ReadOnly, Some("alice")),
            key("sec-b", Permission::ReadOnly, None),
        ]);
        migrate_api_keys(&cfg, &svc).unwrap();
        assert_eq!(
            svc.engine.authenticate_token("sec-a").unwrap().id,
            "alice"
        );
        let derived = svc.engine.authenticate_token("sec-b").unwrap().id;
        assert!(derived.starts_with("key_"), "derived id: {derived}");
        assert_eq!(derived.len(), 4 + 8, "derived id: {derived}");

        // Stable across re-runs (same key → same id, no second principal).
        migrate_api_keys(&cfg, &svc).unwrap();
        assert_eq!(
            svc.engine.authenticate_token("sec-b").unwrap().id,
            derived
        );
        assert_eq!(svc.engine.principals().len(), 2);
    }

    #[test]
    fn shared_principal_unions_tier_and_downgrade_reconciles() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        // Two keys bound to one principal: the union (admin) wins.
        let cfg = config(vec![
            key("laptop", Permission::ReadOnly, Some("alice")),
            key("pipeline", Permission::Admin, Some("alice")),
        ]);
        migrate_api_keys(&cfg, &svc).unwrap();
        let alice = svc.engine.principal("alice").unwrap();
        assert!(alice.roles.contains(BUILTIN_ROLE_ADMIN));
        assert!(!alice.roles.contains(BUILTIN_ROLE_READONLY));
        assert!(!alice.roles.contains(BUILTIN_ROLE_READWRITE));

        // A file edit that drops the admin key downgrades on next start.
        let cfg = config(vec![
            key("laptop", Permission::ReadOnly, Some("alice")),
            key("pipeline", Permission::ReadWrite, Some("alice")),
        ]);
        migrate_api_keys(&cfg, &svc).unwrap();
        let alice = svc.engine.principal("alice").unwrap();
        assert!(alice.roles.contains(BUILTIN_ROLE_READWRITE));
        assert!(!alice.roles.contains(BUILTIN_ROLE_ADMIN));
        assert!(!alice.roles.contains(BUILTIN_ROLE_READONLY));
    }

    // ── design §11 Bootstrap ────────────────────────────────────────────

    #[test]
    fn bootstrap_first_run_mints_admin_token_once() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let out = bootstrap_admin(&svc, None).unwrap();
        assert!(out.audit_detail.contains("created"), "{}", out.audit_detail);
        let token = out.minted_token.expect("first run must mint");
        let admin = svc.engine.principal(BOOTSTRAP_ADMIN_ID).unwrap();
        assert!(admin.roles.contains(BUILTIN_ROLE_ADMIN));
        // The minted token authenticates and is epoch-gated: a credential
        // change (epoch bump) kills it like any session token.
        assert_eq!(
            svc.engine.authenticate_token(&token).unwrap().id,
            BOOTSTRAP_ADMIN_ID
        );
        let mut p = admin;
        p.token_epoch += 1;
        svc.upsert_principal(p).unwrap();
        assert!(svc.engine.authenticate_token(&token).is_none());

        // Later starts are silent no-ops: nothing re-printed, nothing minted.
        let out = bootstrap_admin(&svc, None).unwrap();
        assert!(out.audit_detail.is_empty());
        assert!(out.minted_token.is_none());
    }

    #[test]
    fn bootstrap_injected_token_is_ensure_style_and_unbound() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        // Injected on a fresh store: creates the admin, registers the
        // operator secret, prints nothing.
        let out = bootstrap_admin(&svc, Some("op-secret")).unwrap();
        assert!(out.audit_detail.contains("created"), "{}", out.audit_detail);
        assert!(out.minted_token.is_none());
        assert_eq!(
            svc.engine.authenticate_token("op-secret").unwrap().id,
            BOOTSTRAP_ADMIN_ID
        );
        // Re-asserted at every start (config-as-code), and the secret is not
        // epoch-gated: credential rotation must not orphan the operator's key.
        let out = bootstrap_admin(&svc, Some("op-secret")).unwrap();
        assert!(out.audit_detail.contains("ensured"), "{}", out.audit_detail);
        let mut p = svc.engine.principal(BOOTSTRAP_ADMIN_ID).unwrap();
        p.token_epoch += 1;
        svc.upsert_principal(p).unwrap();
        assert!(svc.engine.authenticate_token("op-secret").is_some());
        // A name collision with an existing principal only adds the token.
        let out = bootstrap_admin(&svc, Some("op-secret-2")).unwrap();
        assert!(out.audit_detail.contains("ensured"), "{}", out.audit_detail);
        assert!(svc.engine.authenticate_token("op-secret-2").is_some());
        assert!(svc.engine.authenticate_token("op-secret").is_some());
    }

    #[test]
    fn bootstrap_skips_when_principals_exist() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        // A migrated key means this is not a fresh store — no auto admin.
        let cfg = config(vec![key("sec-ro", Permission::ReadOnly, None)]);
        migrate_api_keys(&cfg, &svc).unwrap();
        let out = bootstrap_admin(&svc, None).unwrap();
        assert!(out.audit_detail.is_empty());
        assert!(out.minted_token.is_none());
        assert!(svc.engine.principal(BOOTSTRAP_ADMIN_ID).is_none());
    }
}
