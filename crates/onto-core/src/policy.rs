// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! RBAC policy model (M1): principals, roles, grants, credentials, and the
//! policy decision engine (PDP).
//!
//! Design: `docs/RBAC权限模型详细设计.md` (decisions locked 2026-11-26):
//! - credential scheme = **blake3-hmac** (per-user 16B salt + optional server
//!   pepper; `cred_scheme` field allows a future argon2 upgrade);
//! - M1 grant granularity = namespace + class × READ/WRITE/DDL, **no `is_a`
//!   inheritance yet** (M2), no explicit-deny-over-inheritance yet (M2);
//! - namespace grants cascade to the namespace's classes;
//! - evaluation = allow if any role grants, else deny (deny-first precedence
//!   arrives with M2 inheritance);
//! - decisions are cached and invalidated by `role_epoch` (bumped on every
//!   grant/role change).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

// ─── Credentials ───────────────────────────────────────────────────────────

/// Per-user random salt length (design §9.1).
pub const CRED_SALT_LEN: usize = 16;

/// Credential scheme tag stored alongside the hash (design §9.1: keep the
/// door open for argon2 without a migration of the model).
pub const CRED_SCHEME_BLAKE3: &str = "blake3-hmac";

/// Keyed-BLAKE3 password hash: `BLAKE3_keyed(key, password)` where the key
/// mixes the per-user salt with the optional server pepper.
///
/// The pepper (when used) never touches the database; it is supplied at
/// process start (design §9.1). A database-only leak therefore resists an
/// offline dictionary to the extent the pepper stays secret.
pub fn password_hash(password: &str, salt: &[u8; CRED_SALT_LEN], pepper: Option<&[u8]>) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..CRED_SALT_LEN].copy_from_slice(salt);
    if let Some(pepper) = pepper {
        let mix = blake3::hash(pepper);
        for i in 0..CRED_SALT_LEN {
            key[i] ^= mix.as_bytes()[i];
        }
    }
    *blake3::keyed_hash(&key, password.as_bytes()).as_bytes()
}

/// Constant-work password check against a stored hash.
pub fn verify_password(
    password: &str,
    salt: &[u8; CRED_SALT_LEN],
    pepper: Option<&[u8]>,
    expected: &[u8; 32],
) -> bool {
    password_hash(password, salt, pepper)
        .iter()
        .zip(expected.iter())
        .fold(true, |acc, (a, b)| acc & (a == b))
}

/// Token lookup key: tokens are 256-bit random secrets, so a straight
/// BLAKE3 of the presented token is the index (design §9.1: high-entropy
/// tokens need no salt).
pub fn token_hash(token: &str) -> [u8; 32] {
    *blake3::hash(token.as_bytes()).as_bytes()
}

// ─── Policy model ──────────────────────────────────────────────────────────

/// Built-in tier roles (design §9.2): api_keys.json permissions map onto
/// these at migration time. `_admin` membership short-circuits the PDP
/// (design §8 `check` pseudocode).
pub const BUILTIN_ROLE_READONLY: &str = "_readonly";
pub const BUILTIN_ROLE_READWRITE: &str = "_readwrite";
pub const BUILTIN_ROLE_ADMIN: &str = "_admin";

/// M1 action set (INFER joins in M2; ADMIN stays at the HTTP control plane).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    Read,
    Write,
    Ddl,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Read => "READ",
            Action::Write => "WRITE",
            Action::Ddl => "DDL",
        }
    }
}

/// Grant object: a namespace (cascades to its classes) or one class.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectRef {
    Namespace(String),
    Class { ns: String, class: String },
}

impl ObjectRef {
    /// `NAMESPACE x` / `CLASS ns.cls` rendering for SHOW and audit.
    pub fn display(&self) -> String {
        match self {
            ObjectRef::Namespace(ns) => format!("NAMESPACE {ns}"),
            ObjectRef::Class { ns, class } => format!("CLASS {ns}.{class}"),
        }
    }
}

/// One authorization grant: role may `action` on `object`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub role: String,
    pub action: Action,
    pub object: ObjectRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalKind {
    User,
    ServiceAccount,
}

/// An authenticated identity (design §5): user or service account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub kind: PrincipalKind,
    pub roles: HashSet<String>,
    /// Bumped on password change / revocation; tokens minted under an older
    /// epoch die with it (design §9.1).
    pub token_epoch: u64,
    pub disabled: bool,
    /// Credential scheme tag (see [`CRED_SCHEME_BLAKE3`]).
    #[serde(default = "default_cred_scheme")]
    pub cred_scheme: String,
}

fn default_cred_scheme() -> String {
    CRED_SCHEME_BLAKE3.to_string()
}

// ─── Decision engine (PDP) ─────────────────────────────────────────────────

/// Policy decision engine: role→grant sets + principals + a decision cache
/// invalidated by `role_epoch`.
#[derive(Debug, Default)]
pub struct PolicyEngine {
    state: RwLock<PolicyState>,
    epoch: AtomicU64,
}

#[derive(Debug, Default)]
struct PolicyState {
    /// Registered role names (CREATE ROLE / DROP ROLE).
    roles: HashSet<String>,
    /// role → grants (M1: flat list; M2 adds inheritance options).
    grants: HashMap<String, Vec<(Action, ObjectRef)>>,
    /// principal id → principal.
    principals: HashMap<String, Principal>,
    /// principal id → credential (salt+hash), verified on password auth.
    credentials: HashMap<String, CredentialRecord>,
    /// `BLAKE3(token)` → (principal id, mint epoch). The mint epoch is the
    /// principal's `token_epoch` when the token was minted; a token dies once
    /// the principal's epoch moves past it (design §9.1 "在途 token 全失效").
    /// `None` = not epoch-gated (config-managed API keys: their lifetime is
    /// governed by the config file, design §9.2).
    token_index: HashMap<[u8; 32], (String, Option<u64>)>,
    /// Decision cache: (principal id, action, object) → allowed.
    cache: HashMap<(String, Action, ObjectRef), bool>,
}

/// Portable snapshot of the whole policy state, persisted by the
/// engine-backed `PolicyStore` (one WAL-protected row per save).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PolicySnapshot {
    pub principals: Vec<Principal>,
    pub credentials: Vec<CredentialRecord>,
    pub grants: Vec<Grant>,
    /// Registered role names.
    pub roles: Vec<String>,
    /// (hex of `BLAKE3(token)`, principal id)
    pub tokens: Vec<(String, String)>,
    /// (hex of `BLAKE3(token)`, mint epoch) — only epoch-gated tokens appear
    /// here; absent = not epoch-gated. `#[serde(default)]` keeps snapshots
    /// written before epoch tracking readable.
    #[serde(default)]
    pub token_epochs: Vec<(String, u64)>,
}

/// A credential record as persisted in the system namespace (M1.1 storage).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub principal_id: String,
    pub salt: [u8; CRED_SALT_LEN],
    pub hash: [u8; 32],
    #[serde(default = "default_cred_scheme")]
    pub cred_scheme: String,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Export the full state for persistence.
    pub fn export_snapshot(&self) -> PolicySnapshot {
        let st = self.state.read().unwrap();
        PolicySnapshot {
            principals: st.principals.values().cloned().collect(),
            credentials: st.credentials.values().cloned().collect(),
            grants: st
                .grants
                .iter()
                .flat_map(|(role, list)| {
                    list.iter().map(move |(a, o)| Grant {
                        role: role.clone(),
                        action: *a,
                        object: o.clone(),
                    })
                })
                .collect(),
            roles: st.roles.iter().cloned().collect(),
            tokens: st
                .token_index
                .iter()
                .map(|(h, (id, _))| (to_hex(h), id.clone()))
                .collect(),
            token_epochs: st
                .token_index
                .iter()
                .filter_map(|(h, (_, e))| e.map(|e| (to_hex(h), e)))
                .collect(),
        }
    }

    /// Replace the full state from a persisted snapshot.
    pub fn import_snapshot(&self, snap: PolicySnapshot) {
        {
            let mut st = self.state.write().unwrap();
            st.grants.clear();
            st.principals.clear();
            st.credentials.clear();
            st.token_index.clear();
            st.roles = snap.roles.iter().cloned().collect();
            for g in snap.grants {
                st.grants.entry(g.role).or_default().push((g.action, g.object));
            }
            for p in snap.principals {
                st.principals.insert(p.id.clone(), p);
            }
            for c in snap.credentials {
                st.credentials.insert(c.principal_id.clone(), c);
            }
            let epochs: HashMap<String, u64> = snap.token_epochs.into_iter().collect();
            for (h, id) in snap.tokens {
                if let Some(arr) = from_hex(&h) {
                    let mint_epoch = epochs.get(&h).copied();
                    st.token_index.insert(arr, (id, mint_epoch));
                }
            }
        }
        self.bump();
    }

    /// Current role epoch; changes on every mutating grant/principal op.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut st) = self.state.write() {
            st.cache.clear();
        }
    }

    // -- principals --

    pub fn upsert_principal(&self, p: Principal) {
        {
            let mut st = self.state.write().unwrap();
            st.principals.insert(p.id.clone(), p);
        }
        self.bump();
    }

    pub fn remove_principal(&self, id: &str) -> bool {
        let removed = {
            let mut st = self.state.write().unwrap();
            st.token_index.retain(|_, v| v.0 != id);
            st.principals.remove(id).is_some()
        };
        if removed {
            self.bump();
        }
        removed
    }

    pub fn principal(&self, id: &str) -> Option<Principal> {
        self.state.read().unwrap().principals.get(id).cloned()
    }

    pub fn principals(&self) -> Vec<Principal> {
        self.state.read().unwrap().principals.values().cloned().collect()
    }

    /// Register/replace a bearer token for a principal (stores only the hash).
    /// `mint_epoch` is `Some(epoch)` for session tokens minted under that
    /// principal epoch — they die when the principal's epoch moves past them
    /// (password change / revocation, design §9.1). `None` = not epoch-gated
    /// (config-managed API keys, design §9.2: lifetime = config file).
    pub fn put_token(&self, token: &str, principal_id: &str, mint_epoch: Option<u64>) {
        let mut st = self.state.write().unwrap();
        st.token_index
            .insert(token_hash(token), (principal_id.to_string(), mint_epoch));
    }

    /// Resolve a bearer token to its live principal. Fails closed: unknown
    /// token, disabled principal, and epoch-stale session tokens all yield
    /// `None`.
    pub fn authenticate_token(&self, token: &str) -> Option<Principal> {
        let st = self.state.read().unwrap();
        let (id, mint_epoch) = st.token_index.get(&token_hash(token))?;
        let p = st.principals.get(id)?;
        if p.disabled {
            return None;
        }
        if let Some(minted) = mint_epoch {
            if *minted < p.token_epoch {
                return None;
            }
        }
        Some(p.clone())
    }

    /// ROTATE TOKEN (design §10): bump the principal's token epoch — every
    /// epoch-gated token dies — and register `new_token` at the new epoch.
    /// Config-managed keys (not epoch-gated) are unaffected. Returns false
    /// when the principal is unknown.
    pub fn rotate_token(&self, principal_id: &str, new_token: &str) -> bool {
        {
            let mut st = self.state.write().unwrap();
            let Some(p) = st.principals.get_mut(principal_id) else {
                return false;
            };
            p.token_epoch += 1;
            let epoch = p.token_epoch;
            st.token_index
                .insert(token_hash(new_token), (principal_id.to_string(), Some(epoch)));
        }
        self.bump();
        true
    }

    /// Store/replace a principal's password credential (salt+hash).
    pub fn put_credential(&self, rec: CredentialRecord) {
        let mut st = self.state.write().unwrap();
        st.credentials.insert(rec.principal_id.clone(), rec);
    }

    /// Resolve a username+password credential (CLI login).
    /// Fails closed: unknown id, disabled principal, missing credential or
    /// wrong password all yield `None`.
    pub fn authenticate_password(
        &self,
        id: &str,
        password: &str,
        pepper: Option<&[u8]>,
    ) -> Option<Principal> {
        let st = self.state.read().unwrap();
        let p = st.principals.get(id)?;
        if p.disabled {
            return None;
        }
        let cred = st.credentials.get(id)?;
        if cred.cred_scheme != CRED_SCHEME_BLAKE3 {
            return None;
        }
        if !verify_password(password, &cred.salt, pepper, &cred.hash) {
            return None;
        }
        Some(p.clone())
    }

    // -- grants --

    pub fn grant(&self, g: Grant) {
        {
            let mut st = self.state.write().unwrap();
            let list = st.grants.entry(g.role.clone()).or_default();
            if !list.iter().any(|(a, o)| *a == g.action && *o == g.object) {
                list.push((g.action, g.object));
            }
        }
        self.bump();
    }

    pub fn revoke(&self, g: &Grant) -> bool {
        let removed = {
            let mut st = self.state.write().unwrap();
            let mut removed = false;
            if let Some(list) = st.grants.get_mut(&g.role) {
                let before = list.len();
                list.retain(|(a, o)| *a != g.action || *o != g.object);
                removed = list.len() != before;
                if list.is_empty() {
                    st.grants.remove(&g.role);
                }
            }
            removed
        };
        if removed {
            self.bump();
        }
        removed
    }

    pub fn grants_for_role(&self, role: &str) -> Vec<Grant> {
        let st = self.state.read().unwrap();
        st.grants
            .get(role)
            .map(|list| {
                list.iter()
                    .map(|(a, o)| Grant {
                        role: role.to_string(),
                        action: *a,
                        object: o.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn grants(&self) -> Vec<Grant> {
        let st = self.state.read().unwrap();
        st.grants
            .iter()
            .flat_map(|(role, list)| {
                list.iter().map(move |(a, o)| Grant {
                    role: role.clone(),
                    action: *a,
                    object: o.clone(),
                })
            })
            .collect()
    }

    /// Drop every grant of a removed role (DROP ROLE semantics).
    pub fn drop_role(&self, role: &str) {
        {
            let mut st = self.state.write().unwrap();
            st.roles.remove(role);
            st.grants.remove(role);
            for p in st.principals.values_mut() {
                p.roles.remove(role);
            }
        }
        self.bump();
    }

    // -- role registry --

    /// CREATE ROLE: false when the name is taken.
    pub fn create_role(&self, name: &str) -> bool {
        let created = {
            let mut st = self.state.write().unwrap();
            st.roles.insert(name.to_string())
        };
        if created {
            self.bump();
        }
        created
    }

    pub fn role_exists(&self, name: &str) -> bool {
        self.state.read().unwrap().roles.contains(name)
    }

    pub fn roles(&self) -> Vec<String> {
        let mut v: Vec<String> = self.state.read().unwrap().roles.iter().cloned().collect();
        v.sort();
        v
    }

    /// Grant role membership to a principal.
    pub fn grant_role(&self, principal_id: &str, role: &str) -> bool {
        let ok = {
            let mut st = self.state.write().unwrap();
            match st.principals.get_mut(principal_id) {
                Some(p) => {
                    p.roles.insert(role.to_string());
                    true
                }
                None => false,
            }
        };
        if ok {
            self.bump();
        }
        ok
    }

    pub fn revoke_role(&self, principal_id: &str, role: &str) -> bool {
        let ok = {
            let mut st = self.state.write().unwrap();
            match st.principals.get_mut(principal_id) {
                Some(p) => p.roles.remove(role),
                None => false,
            }
        };
        if ok {
            self.bump();
        }
        ok
    }

    // -- decisions --

    /// M1 evaluation: allow iff some role of the principal holds a grant for
    /// the action on the object (namespace grants cascade to classes).
    /// Cached per (principal, action, object); the cache dies with any grant
    /// or membership change.
    pub fn check(&self, principal_id: &str, action: Action, object: &ObjectRef) -> bool {
        let key = (principal_id.to_string(), action, object.clone());
        {
            let st = self.state.read().unwrap();
            if let Some(hit) = st.cache.get(&key) {
                return *hit;
            }
        }
        let allowed = {
            let st = self.state.read().unwrap();
            let Some(p) = st.principals.get(principal_id) else {
                return false;
            };
            if p.disabled {
                return false;
            }
            // Design §8: `_admin` members are allowed unconditionally.
            p.roles.contains(BUILTIN_ROLE_ADMIN)
                || p.roles.iter().any(|role| {
                    st.grants.get(role).is_some_and(|list| {
                        list.iter()
                            .any(|(a, o)| *a == action && grant_covers(o, object))
                    })
                })
        };
        if let Ok(mut st) = self.state.write() {
            st.cache.insert(key, allowed);
        }
        allowed
    }

    /// Convenience for the PEP: check a class touch, honouring namespace
    /// grants (a grant on the namespace covers every class inside it).
    pub fn check_class(
        &self,
        principal_id: &str,
        action: Action,
        ns: &str,
        class: &str,
    ) -> bool {
        self.check(
            principal_id,
            action,
            &ObjectRef::Class {
                ns: ns.to_string(),
                class: class.to_string(),
            },
        )
    }
}

/// Minimal hex helpers for token-hash persistence (no external dep).
fn to_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// M1 object matching: a namespace grant covers its classes; a class grant
/// covers exactly that class; `NAMESPACE *` is the built-in-tier wildcard
/// (design §9.2) covering every namespace and class. (M2 adds `is_a`
/// inheritance and explicit deny-over-inheritance.)
fn grant_covers(grant_obj: &ObjectRef, wanted: &ObjectRef) -> bool {
    match (grant_obj, wanted) {
        (ObjectRef::Namespace(g), _) if g == "*" => true,
        (ObjectRef::Namespace(g), ObjectRef::Namespace(w)) => g == w,
        (ObjectRef::Namespace(g), ObjectRef::Class { ns, .. }) => g == ns,
        (ObjectRef::Class { .. }, ObjectRef::Namespace(_)) => false,
        (ObjectRef::Class { ns: gn, class: gc }, ObjectRef::Class { ns: wn, class: wc }) => {
            gn == wn && gc == wc
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(id: &str, roles: &[&str]) -> Principal {
        Principal {
            id: id.to_string(),
            kind: PrincipalKind::User,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            token_epoch: 0,
            disabled: false,
            cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
        }
    }

    #[test]
    fn password_roundtrip_and_pepper_matters() {
        let salt = [7u8; CRED_SALT_LEN];
        let hash = password_hash("s3cret", &salt, None);
        assert!(verify_password("s3cret", &salt, None, &hash));
        assert!(!verify_password("wrong", &salt, None, &hash));
        // Pepper changes the hash (and verification must use the same one).
        let hash_p = password_hash("s3cret", &salt, Some(b"pepper"));
        assert_ne!(hash, hash_p);
        assert!(verify_password("s3cret", &salt, Some(b"pepper"), &hash_p));
        assert!(!verify_password("s3cret", &salt, None, &hash_p));
    }

    #[test]
    fn token_hash_is_stable_and_lookup_works() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &["analyst"]));
        eng.put_token("tok-1", "alice", Some(0));
        assert!(eng.authenticate_token("tok-1").is_some());
        assert!(eng.authenticate_token("tok-2").is_none());
    }

    #[test]
    fn disabled_and_epoch_stale_fail_closed() {
        let eng = PolicyEngine::new();
        let mut p = principal("bob", &[]);
        p.token_epoch = 3;
        eng.upsert_principal(p);
        eng.put_token("tok-b", "bob", Some(2));
        eng.put_token("tok-c", "bob", Some(3));
        // Session token minted at epoch 2 dies when the principal is at 3
        // (password change / rotation bumps the epoch).
        assert!(eng.authenticate_token("tok-b").is_none());
        assert!(eng.authenticate_token("tok-c").is_some());
        // Disabled fails closed for every token.
        let mut p = eng.principal("bob").unwrap();
        p.disabled = true;
        eng.upsert_principal(p);
        assert!(eng.authenticate_token("tok-c").is_none());
    }

    #[test]
    fn namespace_grant_cascades_to_classes() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &["analyst"]));
        eng.grant(Grant {
            role: "analyst".into(),
            action: Action::Read,
            object: ObjectRef::Namespace("erp".into()),
        });
        assert!(eng.check_class("alice", Action::Read, "erp", "orders"));
        assert!(eng.check(
            "alice",
            Action::Read,
            &ObjectRef::Namespace("erp".into())
        ));
        // Not other namespaces / not writes / not other roles.
        assert!(!eng.check_class("alice", Action::Read, "hr", "people"));
        assert!(!eng.check_class("alice", Action::Write, "erp", "orders"));
        assert!(!eng.check_class("mallory", Action::Read, "erp", "orders"));
    }

    #[test]
    fn class_grant_is_exact_and_role_union() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &["r1", "r2"]));
        eng.grant(Grant {
            role: "r1".into(),
            action: Action::Read,
            object: ObjectRef::Class {
                ns: "erp".into(),
                class: "orders".into(),
            },
        });
        eng.grant(Grant {
            role: "r2".into(),
            action: Action::Write,
            object: ObjectRef::Class {
                ns: "erp".into(),
                class: "orders".into(),
            },
        });
        assert!(eng.check_class("alice", Action::Read, "erp", "orders"));
        assert!(eng.check_class("alice", Action::Write, "erp", "orders"));
        // Class grant does not cascade to siblings or the namespace.
        assert!(!eng.check_class("alice", Action::Read, "erp", "invoices"));
        assert!(!eng.check(
            "alice",
            Action::Read,
            &ObjectRef::Namespace("erp".into())
        ));
    }

    #[test]
    fn revoke_and_drop_role_take_effect() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &["analyst"]));
        let g = Grant {
            role: "analyst".into(),
            action: Action::Read,
            object: ObjectRef::Namespace("erp".into()),
        };
        eng.grant(g.clone());
        assert!(eng.check_class("alice", Action::Read, "erp", "orders"));
        assert!(eng.revoke(&g));
        assert!(!eng.check_class("alice", Action::Read, "erp", "orders"));

        eng.grant(g.clone());
        assert!(eng.check_class("alice", Action::Read, "erp", "orders"));
        eng.drop_role("analyst");
        assert!(!eng.check_class("alice", Action::Read, "erp", "orders"));
        assert!(eng.principal("alice").unwrap().roles.is_empty());
    }

    #[test]
    fn password_auth_roundtrip() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &[]));
        let salt = [7u8; CRED_SALT_LEN];
        eng.put_credential(CredentialRecord {
            principal_id: "alice".into(),
            salt,
            hash: password_hash("s3cret", &salt, None),
            cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
        });
        assert!(eng.authenticate_password("alice", "s3cret", None).is_some());
        assert!(eng.authenticate_password("alice", "wrong", None).is_none());
        assert!(eng.authenticate_password("ghost", "s3cret", None).is_none());
        // Disabled fails closed even with a correct password.
        let mut p = eng.principal("alice").unwrap();
        p.disabled = true;
        eng.upsert_principal(p);
        assert!(eng.authenticate_password("alice", "s3cret", None).is_none());
    }

    #[test]
    fn role_registry_lifecycle() {
        let eng = PolicyEngine::new();
        assert!(eng.create_role("analyst"));
        assert!(!eng.create_role("analyst"), "duplicate rejected");
        assert!(eng.role_exists("analyst"));
        assert_eq!(eng.roles(), vec!["analyst"]);
        eng.drop_role("analyst");
        assert!(!eng.role_exists("analyst"));
        assert!(eng.roles().is_empty());
    }

    #[test]
    fn snapshot_roundtrip() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &["analyst"]));
        eng.put_credential(CredentialRecord {
            principal_id: "alice".into(),
            salt: [1u8; CRED_SALT_LEN],
            hash: [2u8; 32],
            cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
        });
        eng.put_token("tok-1", "alice", None);
        eng.grant(Grant {
            role: "analyst".into(),
            action: Action::Read,
            object: ObjectRef::Namespace("erp".into()),
        });

        let snap = eng.export_snapshot();
        let eng2 = PolicyEngine::new();
        eng2.import_snapshot(snap);
        assert!(eng2.check_class("alice", Action::Read, "erp", "orders"));
        assert!(eng2.authenticate_token("tok-1").is_some());
        assert_eq!(eng2.grants().len(), 1);
    }

    #[test]
    fn cache_invalidation_on_grant_change() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &["r"]));
        let obj = ObjectRef::Class {
            ns: "erp".into(),
            class: "orders".into(),
        };
        assert!(!eng.check("alice", Action::Read, &obj));
        let e0 = eng.epoch();
        eng.grant(Grant {
            role: "r".into(),
            action: Action::Read,
            object: obj.clone(),
        });
        assert!(eng.epoch() > e0);
        assert!(eng.check("alice", Action::Read, &obj));
    }

    #[test]
    fn grant_role_membership_moves_access() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &[]));
        eng.grant(Grant {
            role: "ops".into(),
            action: Action::Ddl,
            object: ObjectRef::Namespace("erp".into()),
        });
        assert!(!eng.check_class("alice", Action::Ddl, "erp", "orders"));
        assert!(eng.grant_role("alice", "ops"));
        assert!(eng.check_class("alice", Action::Ddl, "erp", "orders"));
        assert!(eng.revoke_role("alice", "ops"));
        assert!(!eng.check_class("alice", Action::Ddl, "erp", "orders"));
    }

    #[test]
    fn wildcard_namespace_grant_covers_every_object() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("reader", &["_readonly"]));
        eng.grant(Grant {
            role: "_readonly".into(),
            action: Action::Read,
            object: ObjectRef::Namespace("*".into()),
        });
        assert!(eng.check("reader", Action::Read, &ObjectRef::Namespace("erp".into())));
        assert!(eng.check("reader", Action::Read, &ObjectRef::Namespace("hr".into())));
        assert!(eng.check_class("reader", Action::Read, "erp", "orders"));
        assert!(eng.check_class("reader", Action::Read, "hr", "people"));
        // Wildcard is per action: no WRITE/DDL ride-along.
        assert!(!eng.check_class("reader", Action::Write, "erp", "orders"));
        assert!(!eng.check("reader", Action::Ddl, &ObjectRef::Namespace("erp".into())));
    }

    #[test]
    fn admin_role_short_circuits_without_grants() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("root", &["_admin"]));
        // No grants at all — the `_admin` membership alone allows (design §8).
        assert!(eng.check("root", Action::Ddl, &ObjectRef::Namespace("erp".into())));
        assert!(eng.check_class("root", Action::Write, "any", "class"));
        assert!(eng.check_class("root", Action::Read, "other", "thing"));
        // Losing the membership drops the short-circuit.
        assert!(eng.revoke_role("root", "_admin"));
        assert!(!eng.check_class("root", Action::Read, "any", "class"));
    }

    #[test]
    fn config_tokens_are_not_epoch_gated_but_honor_disabled() {
        let eng = PolicyEngine::new();
        let mut p = principal("svc", &[]);
        p.token_epoch = 5;
        eng.upsert_principal(p);
        eng.put_token("api-key-1", "svc", None);
        // Config keys are not epoch-minted: a bumped epoch does not kill them.
        assert!(eng.authenticate_token("api-key-1").is_some());
        assert_eq!(eng.authenticate_token("api-key-1").unwrap().id, "svc");
        assert!(eng.authenticate_token("unknown").is_none());
        let mut p = eng.principal("svc").unwrap();
        p.disabled = true;
        eng.upsert_principal(p);
        assert!(eng.authenticate_token("api-key-1").is_none());
    }

    #[test]
    fn snapshot_roundtrip_keeps_token_epochs_and_immortality() {
        let eng = PolicyEngine::new();
        let mut p = principal("alice", &[]);
        p.token_epoch = 2;
        eng.upsert_principal(p);
        eng.put_token("session-tok", "alice", Some(2));
        eng.put_token("old-session", "alice", Some(1)); // stale at mint check time
        eng.put_token("config-key", "alice", None);

        let snap = eng.export_snapshot();
        // Only epoch-gated tokens carry a mint epoch in the snapshot.
        assert_eq!(snap.token_epochs.len(), 2);
        let eng2 = PolicyEngine::new();
        eng2.import_snapshot(snap);
        assert!(eng2.authenticate_token("session-tok").is_some());
        assert!(eng2.authenticate_token("old-session").is_none());
        assert!(eng2.authenticate_token("config-key").is_some());
    }

    #[test]
    fn rotate_token_kills_epoch_gated_but_not_config_keys() {
        let eng = PolicyEngine::new();
        eng.upsert_principal(principal("alice", &[]));
        eng.put_token("session-1", "alice", Some(0));
        eng.put_token("config-key", "alice", None);
        assert!(eng.rotate_token("alice", "session-2"));
        // Old epoch-gated token is dead; the replacement and config key live.
        assert!(eng.authenticate_token("session-1").is_none());
        assert!(eng.authenticate_token("session-2").is_some());
        assert!(eng.authenticate_token("config-key").is_some());
        assert_eq!(eng.principal("alice").unwrap().token_epoch, 1);
        // Unknown principal: no-op, false.
        assert!(!eng.rotate_token("ghost", "tok"));
    }
}
