# Multi-Git-Host Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add GitLab and Gitea/Forgejo/Codeberg support to the dkod indexer via per-platform provider crates behind shared Rust traits, with reconciler-only ingest and multi-provider OAuth.

**Architecture:** Per-platform provider crates (`indexer-provider-github`, `indexer-provider-gitlab`, `indexer-provider-gitea`) implement shared traits defined in `indexer-provider`. The reconciler and worker become generic over these traits. Multi-provider OAuth routes dispatch to the correct provider for dashboard sign-in. Content fetch uses the user's OAuth token per platform.

**Tech Stack:** Rust (axum + sqlx + reqwest), Postgres, gitoxide (for `ls-remote`), OAuth2

**Design doc:** `docs/plans/2026-05-07-multi-git-host-design.md`

**Repo:** All changes are in `dkod-indexer` at `~/vsCode/haim-ari/github/dkod-indexer`. The `dkod-cli` and `dkod-app` repos require zero changes.

---

## Phase 1: Foundation — Provider Trait Crate

### Task 1: Create `indexer-provider` crate with shared types

**Files:**
- Create: `crates/indexer-provider/Cargo.toml`
- Create: `crates/indexer-provider/src/lib.rs`
- Modify: `Cargo.toml` (workspace root, add member)

**Step 1: Create the crate directory**

```bash
mkdir -p crates/indexer-provider/src
```

**Step 2: Write `Cargo.toml`**

```toml
[package]
name = "indexer-provider"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true
rust-version.workspace = true

[dependencies]
async-trait.workspace = true
chrono = { workspace = true }
reqwest = { workspace = true }
serde = { workspace = true }
thiserror.workspace = true
url.workspace = true
```

**Step 3: Write `src/lib.rs` with Platform enum, shared types, trait definitions, and ProviderError**

```rust
#![forbid(unsafe_code)]

pub mod error;
pub mod types;
pub mod traits;

pub use error::ProviderError;
pub use types::*;
pub use traits::*;
```

Create `src/error.rs`:

```rust
use reqwest::StatusCode;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("status {status}: {body}")]
    Status { status: StatusCode, body: String },

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("oauth error: {code} — {description}")]
    OAuthError { code: String, description: String },

    #[error("unsupported operation: {0}")]
    Unsupported(String),
}
```

Create `src/types.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Github,
    Gitlab,
    Gitea,
}

impl Platform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Gitea => "gitea",
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Platform {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(Self::Github),
            "gitlab" => Ok(Self::Gitlab),
            "gitea" => Ok(Self::Gitea),
            other => Err(format!("unknown platform: {other}")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RepoCoord {
    pub owner: String,
    pub name: String,
    pub platform_repo_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefTip {
    pub name: String,
    pub oid: String,
}

#[derive(Clone, Debug)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub access_expires_at: Option<DateTime<Utc>>,
    pub refresh_expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct UserInfo {
    pub platform: Platform,
    pub platform_user_id: String,
    pub login: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OrgInfo {
    pub platform_org_id: String,
    pub login: String,
    pub name: Option<String>,
}
```

Create `src/traits.rs`:

```rust
use async_trait::async_trait;

use crate::error::ProviderError;
use crate::types::*;

#[async_trait]
pub trait GitHostClient: Send + Sync + 'static {
    async fn list_dkod_refs(
        &self,
        base_url: &str,
        repo: &RepoCoord,
        credentials: &Credentials,
    ) -> Result<Vec<RefTip>, ProviderError>;

    async fn get_blob(
        &self,
        base_url: &str,
        repo: &RepoCoord,
        credentials: &Credentials,
        oid: &str,
    ) -> Result<Vec<u8>, ProviderError>;
}

#[async_trait]
pub trait UserContentFetcher: Send + Sync + 'static {
    async fn get_blob_as_user(
        &self,
        base_url: &str,
        user_token: &str,
        repo: &RepoCoord,
        oid: &str,
    ) -> Result<Vec<u8>, ProviderError>;
}

#[async_trait]
pub trait OAuthProvider: Send + Sync + 'static {
    fn login_url(
        &self,
        redirect_uri: &str,
        state: &str,
    ) -> Result<String, ProviderError>;

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> Result<TokenPair, ProviderError>;

    async fn refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<TokenPair, ProviderError>;

    async fn get_user(
        &self,
        access_token: &str,
    ) -> Result<UserInfo, ProviderError>;

    async fn get_user_orgs(
        &self,
        access_token: &str,
    ) -> Result<Vec<OrgInfo>, ProviderError>;
}

#[derive(Clone, Debug)]
pub enum Credentials {
    GithubInstallation { installation_id: i64 },
    ServiceToken(String),
}
```

**Step 4: Add to workspace**

In root `Cargo.toml`, add `"crates/indexer-provider"` to the `members` array.

Add to `[workspace.dependencies]`:
```toml
indexer-provider = { path = "crates/indexer-provider" }
```

**Step 5: Verify it compiles**

```bash
cargo check -p indexer-provider
```

**Step 6: Commit**

```bash
git add crates/indexer-provider/ Cargo.toml Cargo.lock
git commit -m "feat: add indexer-provider crate with platform traits and shared types"
```

---

### Task 2: Database migration 0006 — multi-platform columns

**Files:**
- Create: `migrations/0006_multi_platform.sql`

**Step 1: Write the migration**

```sql
-- Multi-platform support: add platform-agnostic columns alongside
-- existing GitHub-specific ones. GitHub columns become nullable;
-- new code uses the generic columns.

-- 1. orgs
ALTER TABLE orgs
    ADD COLUMN platform TEXT NOT NULL DEFAULT 'github',
    ADD COLUMN platform_org_id TEXT,
    ADD COLUMN base_url TEXT;

UPDATE orgs SET platform_org_id = github_org_id::TEXT;
ALTER TABLE orgs ALTER COLUMN platform_org_id SET NOT NULL;
ALTER TABLE orgs ALTER COLUMN github_org_id DROP NOT NULL;

CREATE UNIQUE INDEX orgs_platform_uniq
    ON orgs (platform, platform_org_id, COALESCE(base_url, ''));

-- 2. users
ALTER TABLE users
    ADD COLUMN platform TEXT NOT NULL DEFAULT 'github',
    ADD COLUMN platform_user_id TEXT;

UPDATE users SET platform_user_id = github_user_id::TEXT;
ALTER TABLE users ALTER COLUMN platform_user_id SET NOT NULL;
ALTER TABLE users ALTER COLUMN github_user_id DROP NOT NULL;

CREATE UNIQUE INDEX users_platform_uniq
    ON users (platform, platform_user_id);

-- 3. connections (non-GitHub equivalent of installations)
CREATE TABLE connections (
    id                UUID PRIMARY KEY,
    org_id            UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    platform          TEXT NOT NULL,
    base_url          TEXT,
    service_token_enc BYTEA,
    label             TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (id, org_id)
);

-- 4. repos
ALTER TABLE repos
    ADD COLUMN platform TEXT NOT NULL DEFAULT 'github',
    ADD COLUMN platform_repo_id TEXT,
    ADD COLUMN base_url TEXT,
    ADD COLUMN connection_id UUID REFERENCES connections(id) ON DELETE CASCADE;

UPDATE repos SET platform_repo_id = github_repo_id::TEXT;
ALTER TABLE repos ALTER COLUMN platform_repo_id SET NOT NULL;
ALTER TABLE repos ALTER COLUMN github_repo_id DROP NOT NULL;
ALTER TABLE repos ALTER COLUMN installation_id DROP NOT NULL;

CREATE UNIQUE INDEX repos_platform_uniq
    ON repos (platform, platform_repo_id, COALESCE(base_url, ''));

-- 5. user_tokens: support tokens for multiple platforms per user
ALTER TABLE user_tokens DROP CONSTRAINT user_tokens_pkey;
ALTER TABLE user_tokens
    ADD COLUMN platform TEXT NOT NULL DEFAULT 'github';
ALTER TABLE user_tokens
    ADD PRIMARY KEY (user_id, platform);
```

**Step 2: Regenerate sqlx offline data**

Since `dkod-indexer` uses `sqlx::query!()` macros with offline checking (the `.sqlx/` directory), after adding the migration you need a running Postgres to regenerate:

```bash
# If DATABASE_URL is set and Postgres is running:
cargo sqlx prepare --workspace
```

If offline, the CI will catch any query mismatches. The plan handles query updates in subsequent tasks.

**Step 3: Commit**

```bash
git add migrations/0006_multi_platform.sql
git commit -m "feat: add migration 0006 for multi-platform support"
```

---

### Task 3: Rename `indexer-github` → `indexer-provider-github`

**Files:**
- Rename: `crates/indexer-github/` → `crates/indexer-provider-github/`
- Modify: `crates/indexer-provider-github/Cargo.toml` (rename package)
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/indexer-ingest/Cargo.toml` (update dep name)
- Modify: `crates/indexer-server/Cargo.toml` (update dep name)
- Modify: all `use indexer_github::` imports across the workspace

**Step 1: Rename the directory**

```bash
mv crates/indexer-github crates/indexer-provider-github
```

**Step 2: Update `crates/indexer-provider-github/Cargo.toml`**

Change `name = "indexer-github"` to `name = "indexer-provider-github"`.

Add dependency on `indexer-provider`:
```toml
indexer-provider.workspace = true
```

**Step 3: Update workspace root `Cargo.toml`**

Change `"crates/indexer-github"` to `"crates/indexer-provider-github"` in members.

Update `[workspace.dependencies]`:
```toml
# Remove: indexer-github = ...
# (other crates reference it by path in their own Cargo.toml)
```

**Step 4: Update downstream crate Cargo.tomls**

In `crates/indexer-ingest/Cargo.toml`: change `indexer-github` to `indexer-provider-github`.
In `crates/indexer-server/Cargo.toml`: change `indexer-github` to `indexer-provider-github`.

**Step 5: Update all `use` statements**

Find and replace across the workspace:
- `use indexer_github::` → `use indexer_provider_github::`
- `indexer_github::` → `indexer_provider_github::` (in paths)

Key files to update:
- `crates/indexer-ingest/src/reconciler.rs` (line 46: `use indexer_github::`)
- `crates/indexer-ingest/src/worker.rs` (line 35: `use indexer_github::`)
- `crates/indexer-server/src/main.rs` (lines 28–29: `use indexer_github::`)
- `crates/indexer-server/src/webhooks.rs` (line 42: `use indexer_github::`)
- `crates/indexer-server/src/auth/routes.rs` (lines 23–27: `use indexer_github::`)
- `crates/indexer-server/src/auth/state.rs` (line 25: `use indexer_github::`)

**Step 6: Verify compilation**

```bash
cargo check --workspace
cargo test --workspace
```

**Step 7: Commit**

```bash
git add -A
git commit -m "refactor: rename indexer-github to indexer-provider-github"
```

---

### Task 4: Implement provider traits on the GitHub provider

**Files:**
- Modify: `crates/indexer-provider-github/Cargo.toml` (add `indexer-provider` dep)
- Create: `crates/indexer-provider-github/src/provider.rs`
- Modify: `crates/indexer-provider-github/src/lib.rs` (add module + re-export)

**Step 1: Write `provider.rs` — GitHub implementations of all three traits**

The `GithubProvider` struct wraps the existing `GhClient` (for installation-token operations) and an `reqwest::Client` + OAuth config (for user-token operations). It implements `GitHostClient`, `UserContentFetcher`, and `OAuthProvider` by delegating to the existing functions.

```rust
use async_trait::async_trait;
use indexer_provider::{
    Credentials, GitHostClient, OAuthProvider, OrgInfo, ProviderError,
    RefTip, RepoCoord, TokenPair, UserContentFetcher, UserInfo, Platform,
};
use std::sync::Arc;

use crate::installation::{GhClient, JwtSigner};
use crate::oauth;
use crate::refs;
use crate::user_api;

pub struct GithubProvider {
    gh_client: GhClient,
    http: reqwest::Client,
    oauth_client_id: String,
    oauth_client_secret: String,
    oauth_host: Option<String>,
    api_base: String,
}

impl GithubProvider {
    pub fn new(
        gh_client: GhClient,
        http: reqwest::Client,
        oauth_client_id: String,
        oauth_client_secret: String,
        oauth_host: Option<String>,
        api_base: String,
    ) -> Self {
        Self { gh_client, http, oauth_client_id, oauth_client_secret, oauth_host, api_base }
    }

    pub fn gh_client(&self) -> &GhClient { &self.gh_client }
}
```

**`GitHostClient` impl:** Delegate to existing `GhClient::list_refs` + `GhClient::get_blob`, extracting `installation_id` from `Credentials::GithubInstallation`.

**`UserContentFetcher` impl:** Issue `GET {api_base}/repos/{owner}/{name}/git/blobs/{oid}` with the user's bearer token. Use `Accept: application/vnd.github.raw+json` for raw bytes (same as `content.rs:122`).

**`OAuthProvider` impl:** Delegate to existing `oauth::login_url`, `oauth::exchange_code`, `oauth::refresh_user_token`, `user_api::get_authenticated_user`, `user_api::get_user_installations`. Map `GhError` to `ProviderError`.

**Step 2: Add module to `lib.rs`**

Add `pub mod provider;` and re-export `pub use provider::GithubProvider;`.

**Step 3: Verify**

```bash
cargo check -p indexer-provider-github
```

**Step 4: Commit**

```bash
git add crates/indexer-provider-github/
git commit -m "feat: implement provider traits on GithubProvider"
```

---

### Task 5: Create `ProviderRegistry` and wire into `AppState`

**Files:**
- Create: `crates/indexer-provider/src/registry.rs`
- Modify: `crates/indexer-provider/src/lib.rs`
- Modify: `crates/indexer-server/src/auth/state.rs` (add registry to `AppState`)
- Modify: `crates/indexer-server/src/main.rs` (construct registry at boot)
- Modify: `crates/indexer-server/Cargo.toml` (add `indexer-provider` dep)

**Step 1: Write `registry.rs`**

```rust
use std::sync::Arc;
use crate::{GitHostClient, OAuthProvider, Platform, ProviderError, UserContentFetcher};

pub struct ProviderRegistry {
    providers: Vec<(Platform, Arc<dyn ProviderBundle>)>,
}

pub trait ProviderBundle: Send + Sync + 'static {
    fn git_host(&self) -> &dyn GitHostClient;
    fn user_fetcher(&self) -> &dyn UserContentFetcher;
    fn oauth(&self) -> &dyn OAuthProvider;
}

impl ProviderRegistry {
    pub fn new() -> Self { Self { providers: Vec::new() } }

    pub fn register(&mut self, platform: Platform, bundle: Arc<dyn ProviderBundle>) {
        self.providers.push((platform, bundle));
    }

    pub fn git_host(&self, platform: Platform) -> Result<&dyn GitHostClient, ProviderError> {
        self.lookup(platform).map(|b| b.git_host())
    }

    pub fn user_fetcher(&self, platform: Platform) -> Result<&dyn UserContentFetcher, ProviderError> {
        self.lookup(platform).map(|b| b.user_fetcher())
    }

    pub fn oauth(&self, platform: Platform) -> Result<&dyn OAuthProvider, ProviderError> {
        self.lookup(platform).map(|b| b.oauth())
    }

    fn lookup(&self, platform: Platform) -> Result<&dyn ProviderBundle, ProviderError> {
        self.providers
            .iter()
            .find(|(p, _)| *p == platform)
            .map(|(_, b)| b.as_ref())
            .ok_or_else(|| ProviderError::Unsupported(format!("platform {platform} not configured")))
    }
}
```

**Step 2: Implement `ProviderBundle` for `GithubProvider`**

In `crates/indexer-provider-github/src/provider.rs`, add:

```rust
impl indexer_provider::registry::ProviderBundle for GithubProvider {
    fn git_host(&self) -> &dyn GitHostClient { self }
    fn user_fetcher(&self) -> &dyn UserContentFetcher { self }
    fn oauth(&self) -> &dyn OAuthProvider { self }
}
```

**Step 3: Add `registry: Arc<ProviderRegistry>` to `AppState`**

In `crates/indexer-server/src/auth/state.rs`, add:
```rust
use indexer_provider::registry::ProviderRegistry;
// ...
pub registry: Arc<ProviderRegistry>,
```

**Step 4: Construct the registry in `main.rs`**

After building `GhClient`, construct `GithubProvider`, wrap in `Arc`, register with `ProviderRegistry`, and pass to `AppState`.

**Step 5: Verify**

```bash
cargo check --workspace
```

**Step 6: Commit**

```bash
git add -A
git commit -m "feat: add ProviderRegistry and wire into AppState"
```

---

## Phase 2: Generic Reconciler and Worker

### Task 6: Make the reconciler generic over `GitHostClient`

**Files:**
- Modify: `crates/indexer-ingest/src/reconciler.rs`
- Modify: `crates/indexer-ingest/src/lib.rs`
- Modify: `crates/indexer-ingest/Cargo.toml` (add `indexer-provider` dep)
- Modify: `crates/indexer-server/src/main.rs` (pass registry to reconciler)

**Step 1: Update the reconciler's `walk_repos_once` signature**

Currently (line 112):
```rust
pub async fn walk_repos_once(pool: &PgPool, gh: &GhClient) -> Result<u64, sqlx::Error>
```

Change to:
```rust
pub async fn walk_repos_once(
    pool: &PgPool,
    registry: &ProviderRegistry,
) -> Result<u64, sqlx::Error>
```

**Step 2: Update the SQL query (line 115–118)**

Currently joins `repos` with `installations` to get `github_installation_id`. Update to also join `connections` and include `platform`, `base_url`, `connection_id`:

```sql
SELECT r.id, r.org_id, r.platform, r.full_name, r.base_url,
       i.id AS install_uuid, i.github_installation_id,
       c.id AS conn_uuid, c.service_token_enc
FROM repos r
LEFT JOIN installations i ON i.id = r.installation_id
LEFT JOIN connections c ON c.id = r.connection_id
```

**Step 3: Update the per-repo loop**

For each row, determine `Platform` from `r.platform`, build the appropriate `Credentials` (GitHub installation or service token), and call `registry.git_host(platform)?.list_dkod_refs(...)`.

For the service token, decrypt it using the encryption key (which means the reconciler needs access to the `SecretKey` — add it as a parameter or pass a pre-built `Connection` struct).

**Step 4: Update `run()` to accept `ProviderRegistry` instead of `GhClient`**

```rust
pub async fn run(pool: PgPool, registry: Arc<ProviderRegistry>, shutdown: CancellationToken)
```

**Step 5: Update `main.rs` — pass registry to reconciler**

```rust
let reconciler_handle = tokio::spawn(reconciler::run(
    pool.clone(), registry.clone(), shutdown.clone(),
));
```

**Step 6: Update tests**

The reconciler tests currently create a `GhClient` with wiremock. They need to be updated to create a `GithubProvider` wrapped in a `ProviderRegistry`. The test setup helpers (`gh_client_with_token`, `seed`) stay largely the same — the wiremock approach still works.

**Step 7: Verify**

```bash
cargo test -p indexer-ingest
```

**Step 8: Commit**

```bash
git add -A
git commit -m "feat: make reconciler generic over ProviderRegistry"
```

---

### Task 7: Make the worker generic over `GitHostClient`

**Files:**
- Modify: `crates/indexer-ingest/src/worker.rs`
- Modify: `crates/indexer-server/src/main.rs`

**Step 1: Update `process_job` signature**

Currently (line 234):
```rust
pub async fn process_job(pool: &PgPool, gh: &GhClient, job: &IngestJob) -> Result<(), ProcessError>
```

Change to:
```rust
pub async fn process_job(
    pool: &PgPool,
    registry: &ProviderRegistry,
    job: &IngestJob,
) -> Result<(), ProcessError>
```

**Step 2: Update `lookup_repo_coords` (line 180)**

Currently returns `(i64, String, String)` — `(github_installation_id, owner, name)`. Update to also return `platform`, `base_url`, and credentials:

```rust
async fn lookup_repo_coords(pool: &PgPool, repo_id: Uuid) -> Result<RepoContext, ProcessError>
```

Where `RepoContext` includes the platform, base_url, credentials, and repo coord.

**Step 3: Update the blob fetch call (line 248)**

Replace:
```rust
let bytes = gh.get_blob(install_id, &repo, &job.ref_tip).await?;
```

With:
```rust
let client = registry.git_host(ctx.platform)?;
let bytes = client.get_blob(&ctx.base_url, &ctx.repo, &ctx.credentials, &job.ref_tip).await?;
```

**Step 4: Update `agent_label` (line 219)**

Currently hardcodes `Agent::ClaudeCode` and `Agent::Codex`. This is driven by `dkod-core::session::Agent` — no change needed here since it's about the AI agent, not the git host.

**Step 5: Update `run()`, `try_process_job()` signatures**

Change `gh: GhClient` to `registry: Arc<ProviderRegistry>` throughout the call chain.

**Step 6: Update `main.rs`**

```rust
let worker_handle = tokio::spawn(worker::run(
    pool.clone(), registry.clone(), shutdown.clone(),
));
```

**Step 7: Update tests**

Same pattern as Task 6 — wrap `GithubProvider` in `ProviderRegistry` for the test helpers.

**Step 8: Verify**

```bash
cargo test -p indexer-ingest
```

**Step 9: Commit**

```bash
git add -A
git commit -m "feat: make worker generic over ProviderRegistry"
```

---

### Task 8: Remove the `push` webhook handler

**Files:**
- Modify: `crates/indexer-server/src/webhooks.rs`

**Step 1: Remove `handle_push` and `enqueue_job`**

In `webhooks.rs`, remove:
- `handle_push()` function (lines 169–198)
- `enqueue_job()` function (lines 205–233)
- `PushEvent` struct (lines 97–108)
- `RepositoryRef` struct (lines 105–108)
- `DKOD_REF_PREFIX` constant (line 58)

**Step 2: Update the `handle_webhook` match arm**

Change the `"push"` match arm from calling `handle_push` to returning `StatusCode::NO_CONTENT` with a log:

```rust
"push" => {
    tracing::debug!("push webhook received — reconciler handles ref discovery; no-op");
    StatusCode::NO_CONTENT
}
```

**Step 3: Remove push-related tests**

Remove tests: `enqueues_ingest_job_for_dkod_ref`, `ignores_non_dkod_refs`, `drops_unknown_repo_silently`, `rejects_malformed_json` (the JSON test only tested push parsing).

Keep: `rejects_bad_signature`, `rejects_missing_signature`, `ping_event_returns_no_content`, `ping_event_with_bad_signature_still_401`, all `installation*` tests, `unknown_event_returns_204`.

**Step 4: Verify**

```bash
cargo test -p indexer-server
```

**Step 5: Commit**

```bash
git add -A
git commit -m "refactor: remove push webhook handler — reconciler is sole ingest path"
```

---

## Phase 3: Multi-Provider OAuth

### Task 9: Add multi-provider OAuth routes

**Files:**
- Modify: `crates/indexer-server/src/auth/routes.rs`
- Modify: `crates/indexer-server/src/main.rs` (router)

**Step 1: Add platform-parameterized login route**

Currently `GET /auth/github/login` is hardcoded. Add:

```text
GET /auth/login/:platform  → generate CSRF state, redirect to platform's OAuth
```

The handler extracts `platform` from the path, looks up the `OAuthProvider` from the registry, calls `provider.login_url(redirect_uri, state)`.

The redirect URI includes the platform: `/auth/callback/{platform}`.

**Step 2: Add platform-parameterized callback route**

```text
GET /auth/callback/:platform → verify state, exchange code, fetch user, persist, issue JWT
```

The handler:
1. Extracts `platform` from path
2. Looks up `OAuthProvider` from registry
3. Calls `provider.exchange_code(code, redirect_uri)`
4. Calls `provider.get_user(access_token)` → `UserInfo`
5. Calls `provider.get_user_orgs(access_token)` → `Vec<OrgInfo>`
6. UPSERTs `users` row using `platform` + `platform_user_id` instead of `github_user_id`
7. UPSERTs `orgs` rows using `platform` + `platform_org_id`
8. Stores token with `(user_id, platform)` key in `user_tokens`
9. Issues session JWT, redirects to dashboard

**Step 3: Keep legacy `/auth/github/login` and `/auth/github/callback` as aliases**

For backward compatibility, keep them as thin redirects or duplicates pointing at the same handlers with `platform = "github"`.

**Step 4: Update `upsert_user`, `upsert_org` helpers**

Currently use `github_user_id` / `github_org_id`. Add parallel branches that use `platform` + `platform_user_id` / `platform_org_id`, with `ON CONFLICT (platform, platform_user_id)`.

**Step 5: Update `store_user_token`**

Currently keyed by `user_id` only. Add `platform` parameter:

```rust
pub async fn store_user_token(
    pool: &PgPool, key: &SecretKey, user_id: Uuid, platform: &str, token: &UserToken,
) -> Result<(), sqlx::Error>
```

The INSERT now uses `(user_id, platform)` as the conflict target.

**Step 6: Update `load_user_token`**

Add `platform` parameter:

```rust
pub async fn load_user_token(
    pool: &PgPool, key: &SecretKey, user_id: Uuid, platform: &str,
) -> Result<Option<UserToken>, anyhow::Error>
```

**Step 7: Verify**

```bash
cargo check -p indexer-server
```

**Step 8: Commit**

```bash
git add -A
git commit -m "feat: add multi-provider OAuth login and callback routes"
```

---

### Task 10: Make the content proxy platform-aware

**Files:**
- Modify: `crates/indexer-server/src/api/content.rs`

**Step 1: Update `sessions_content_handler`**

Currently (line 67–200) hardcodes GitHub blob URL construction and uses `state.github_api_base`. Update to:

1. Look up the session's repo including `platform` and `base_url`:
   ```sql
   SELECT s.repo_id, s.blob_sha, r.full_name, r.platform, r.base_url
   FROM sessions s JOIN repos r ON r.id = s.repo_id AND r.org_id = s.org_id
   WHERE s.id = $1 AND s.org_id = $2
   ```

2. Load the user's token for the repo's platform:
   ```rust
   let token = load_user_token(&state.pool, &state.token_enc_key, cu.user_id, &platform).await?;
   ```

3. Fetch blob through the registry:
   ```rust
   let fetcher = state.registry.user_fetcher(platform)?;
   let bytes = fetcher.get_blob_as_user(&base_url, &token.access_token, &repo, &blob_sha).await?;
   ```

4. Return the bytes as a response body (can't stream through the trait — the trait returns `Vec<u8>`). For V1 this is acceptable; streaming can be added later as a trait method returning `impl Stream`.

**Step 2: Verify**

```bash
cargo test -p indexer-server
```

**Step 3: Commit**

```bash
git add -A
git commit -m "feat: make content proxy platform-aware via ProviderRegistry"
```

---

## Phase 4: GitLab Provider

### Task 11: Create `indexer-provider-gitlab` crate

**Files:**
- Create: `crates/indexer-provider-gitlab/Cargo.toml`
- Create: `crates/indexer-provider-gitlab/src/lib.rs`
- Create: `crates/indexer-provider-gitlab/src/provider.rs`
- Create: `crates/indexer-provider-gitlab/src/oauth.rs`
- Create: `crates/indexer-provider-gitlab/src/refs.rs`
- Modify: `Cargo.toml` (workspace root)

**Step 1: Scaffold the crate**

```bash
mkdir -p crates/indexer-provider-gitlab/src
```

**Step 2: Write `Cargo.toml`**

```toml
[package]
name = "indexer-provider-gitlab"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true
rust-version.workspace = true

[dependencies]
indexer-provider.workspace = true
async-trait.workspace = true
reqwest = { workspace = true }
serde = { workspace = true }
serde_json.workspace = true
url.workspace = true
chrono = { workspace = true }
base64.workspace = true
tracing.workspace = true

[dev-dependencies]
wiremock.workspace = true
tokio = { workspace = true }
```

**Step 3: Implement `GitHostClient` for GitLab**

- `list_dkod_refs`: Use `git ls-remote` over HTTPS. Build clone URL as `https://oauth2:{service_token}@{host}/{owner}/{name}.git`. Shell out to `git ls-remote --refs {url} refs/dkod/*` or use reqwest to speak the git smart HTTP protocol directly. For V1, shelling out to `git` is simpler and reliable.
- `get_blob`: `GET /api/v4/projects/{url_encoded_path}/repository/blobs/{sha}/raw` with `PRIVATE-TOKEN: {service_token}` header.

**Step 4: Implement `OAuthProvider` for GitLab**

- `login_url`: `https://{host}/oauth/authorize?client_id=...&redirect_uri=...&response_type=code&state=...&scope=read_api+read_user`
- `exchange_code`: `POST https://{host}/oauth/token` with `grant_type=authorization_code`
- `refresh_token`: `POST https://{host}/oauth/token` with `grant_type=refresh_token`
- `get_user`: `GET /api/v4/user` with bearer token
- `get_user_orgs`: `GET /api/v4/groups?min_access_level=10` with bearer token

**Step 5: Implement `UserContentFetcher` for GitLab**

`GET /api/v4/projects/{url_encoded_path}/repository/blobs/{sha}/raw` with user's OAuth bearer token.

**Step 6: Implement `ProviderBundle`**

Same pattern as GitHub — `GitlabProvider` implements all three traits and `ProviderBundle`.

**Step 7: Write tests with wiremock**

Test each trait method with a wiremock server — same pattern as `indexer-provider-github` tests.

**Step 8: Add to workspace and verify**

```bash
cargo test -p indexer-provider-gitlab
```

**Step 9: Commit**

```bash
git add -A
git commit -m "feat: add GitLab provider (indexer-provider-gitlab)"
```

---

### Task 12: Wire GitLab into the registry

**Files:**
- Modify: `crates/indexer-server/src/main.rs`
- Modify: `crates/indexer-server/Cargo.toml`

**Step 1: Add optional GitLab env vars to `Config`**

```rust
gitlab_client_id: Option<String>,
gitlab_client_secret: Option<String>,
gitlab_base_url: Option<String>,  // defaults to https://gitlab.com
```

**Step 2: Construct `GitlabProvider` if configured**

In `main()`, after building the GitHub provider:

```rust
if let (Some(client_id), Some(client_secret)) = (&cfg.gitlab_client_id, &cfg.gitlab_client_secret) {
    let gitlab = GitlabProvider::new(http.clone(), client_id, client_secret, gitlab_base_url);
    registry.register(Platform::Gitlab, Arc::new(gitlab));
}
```

**Step 3: Verify — GitLab is optional**

When `GITLAB_CLIENT_ID` is unset, the registry simply has no GitLab provider. Any request for `Platform::Gitlab` returns `ProviderError::Unsupported`.

```bash
cargo check --workspace
```

**Step 4: Commit**

```bash
git add -A
git commit -m "feat: wire GitLab provider into registry (optional)"
```

---

## Phase 5: Gitea/Forgejo/Codeberg Provider

### Task 13: Create `indexer-provider-gitea` crate

**Files:**
- Create: `crates/indexer-provider-gitea/Cargo.toml`
- Create: `crates/indexer-provider-gitea/src/lib.rs`
- Create: `crates/indexer-provider-gitea/src/provider.rs`
- Modify: `Cargo.toml` (workspace root)

Same structure as Task 11 but targeting the Gitea API v1:

**`GitHostClient` impl:**
- `list_dkod_refs`: `git ls-remote` over HTTPS (same approach as GitLab)
- `get_blob`: `GET /api/v1/repos/{owner}/{name}/git/blobs/{sha}` — returns `{ content, encoding }` (base64), decode like the GitHub provider does

**`OAuthProvider` impl:**
- `login_url`: `https://{host}/login/oauth/authorize?client_id=...&redirect_uri=...&response_type=code&state=...`
- `exchange_code`: `POST {host}/login/oauth/access_token`
- `refresh_token`: `POST {host}/login/oauth/access_token` with `grant_type=refresh_token`
- `get_user`: `GET /api/v1/user`
- `get_user_orgs`: `GET /api/v1/user/orgs`

**`UserContentFetcher` impl:**
- `GET /api/v1/repos/{owner}/{name}/git/blobs/{sha}` with user's OAuth bearer token, decode base64

**Step 1–8:** Same pattern as Task 11.

**Step 9: Commit**

```bash
git add -A
git commit -m "feat: add Gitea provider (indexer-provider-gitea) — covers Forgejo + Codeberg"
```

---

### Task 14: Wire Gitea into the registry

Same pattern as Task 12. Optional env vars `GITEA_CLIENT_ID`, `GITEA_CLIENT_SECRET`, `GITEA_BASE_URL`.

```bash
git commit -m "feat: wire Gitea provider into registry (optional)"
```

---

## Phase 6: Connection Management API

### Task 15: Add connection management endpoints

**Files:**
- Create: `crates/indexer-server/src/api/connections.rs`
- Modify: `crates/indexer-server/src/api/mod.rs`
- Modify: `crates/indexer-server/src/main.rs` (add routes)

**Step 1: Implement endpoints**

```text
POST   /api/connections              — create a connection (platform, base_url, service_token, label)
GET    /api/connections              — list connections for user's org
DELETE /api/connections/:id          — delete a connection (CASCADE repos)
POST   /api/connections/:id/validate — test the service token
GET    /api/connections/:id/available-repos — list repos accessible via the token
POST   /api/connections/:id/repos    — add selected repos
DELETE /api/connections/:id/repos/:repo_id — remove a repo
```

**`POST /api/connections`:**
1. Validate platform is supported
2. Encrypt service token with `SecretKey`
3. INSERT into `connections` table
4. UPSERT org if needed (for the service token's owner)

**`POST /api/connections/:id/validate`:**
1. Decrypt service token
2. Call `registry.git_host(platform)?.list_dkod_refs(...)` on a dummy/first repo
3. Return success/failure

**`GET /api/connections/:id/available-repos`:**
1. Decrypt service token
2. Call the platform API to list repos accessible via the token
3. Return the list for the UI picker

**`POST /api/connections/:id/repos`:**
1. For each selected repo, INSERT into `repos` with `connection_id`, `platform`, `platform_repo_id`

**Step 2: Add routes to `build_router`**

All behind `require_session` middleware.

**Step 3: Write tests**

Test happy path + auth + tenant isolation.

**Step 4: Commit**

```bash
git add -A
git commit -m "feat: add connection management API endpoints"
```

---

## Phase 7: Integration Testing

### Task 16: End-to-end integration test

**Files:**
- Create: `tests/integration/multi_platform.rs` (or add to existing test harness)

**Step 1: Write an integration test**

1. Boot the server with all three providers configured (GitHub + GitLab + Gitea, all pointed at wiremock)
2. Simulate GitHub App install webhook → reconciler polls → session indexed
3. Create a GitLab connection via API → add repos → reconciler polls → session indexed
4. Sign in via GitHub OAuth → view GitHub session content
5. Sign in via GitLab OAuth → view GitLab session content
6. Verify cross-platform tenant isolation (GitHub user can't see GitLab sessions from another org)

**Step 2: Verify**

```bash
cargo test --test multi_platform
```

**Step 3: Commit**

```bash
git add -A
git commit -m "test: add multi-platform integration tests"
```

---

## Summary

| Phase | Tasks | What it delivers |
|-------|-------|-----------------|
| 1 — Foundation | 1–5 | Provider crate, DB migration, trait impls, registry |
| 2 — Generic ingest | 6–8 | Reconciler + worker use traits, push webhook removed |
| 3 — Multi-provider auth | 9–10 | Platform-parameterized OAuth, platform-aware content proxy |
| 4 — GitLab | 11–12 | GitLab provider crate, wired into registry |
| 5 — Gitea | 13–14 | Gitea provider crate (covers Forgejo + Codeberg) |
| 6 — Connections | 15 | Connection CRUD API for non-GitHub platforms |
| 7 — Integration | 16 | End-to-end verification |

**Checkpoint after Phase 2:** GitHub path works exactly as before through the new trait layer. All existing tests pass. This is the safe point to deploy and verify no regressions.

**Checkpoint after Phase 3:** Multi-provider OAuth works. Users can sign in via any configured provider.

**Checkpoint after Phase 5:** GitLab and Gitea repos can be connected, polled, and browsed. Ship candidate.
