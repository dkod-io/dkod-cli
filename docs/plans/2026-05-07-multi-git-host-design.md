# Multi-Git-Host Support — Design

**Date:** 2026-05-07
**Status:** approved, ready for implementation planning

## TL;DR

The dkod indexer and dashboard are currently GitHub-only. This design adds a
platform abstraction layer so the indexer can poll and index sessions from
GitHub, GitLab, and Gitea/Forgejo/Codeberg at launch, with the architecture
supporting any future git host without touching core logic. The CLI and
desktop app require zero changes.

## Decisions locked

| Decision | Choice |
|---|---|
| Ship scope | GitHub + GitLab + Gitea family at launch; architecture supports all hosts |
| Ingest strategy | Reconciler-only (polling via `git ls-remote`). Webhooks removed as ingest path |
| Dashboard auth | Multi-provider OAuth (sign in with GitHub / GitLab / Gitea) |
| Indexer repo access | GitHub: existing App model (installation tokens). Others: admin-provided service token |
| Content fetch | Always through the user's OAuth token (privacy model preserved) |
| Self-hosted content fetch (V1) | Falls back to service token; full OAuth in V1.5 |
| User identity | Separate identities per platform for V1; linked identities in V1.5 |
| CLI changes | None |
| Architecture | Per-platform provider crates behind shared Rust traits |

## Architecture

### Provider trait surface

Three traits in a new `indexer-provider` crate. Every git host provider
implements all three.

#### `GitHostClient` — reconciler + worker

```rust
#[async_trait]
pub trait GitHostClient: Send + Sync + Clone + 'static {
    async fn list_dkod_refs(&self, conn: &Connection) -> Result<Vec<RefTip>, ProviderError>;
    async fn get_blob(&self, conn: &Connection, oid: &str) -> Result<Vec<u8>, ProviderError>;
}
```

`list_dkod_refs` uses `git ls-remote` over HTTPS for all platforms — the one
operation every git host supports identically. `get_blob` calls a per-platform
REST endpoint to fetch blob content by SHA.

#### `UserContentFetcher` — dashboard content proxy

```rust
#[async_trait]
pub trait UserContentFetcher: Send + Sync + 'static {
    async fn get_blob_as_user(
        &self,
        user_token: &str,
        repo: &RepoCoord,
        oid: &str,
    ) -> Result<Vec<u8>, ProviderError>;
}
```

Fetches blob content using the requesting user's OAuth token. The user's git
host permissions decide what they can see — transcripts are never persisted.

#### `OAuthProvider` — dashboard auth

```rust
#[async_trait]
pub trait OAuthProvider: Send + Sync + 'static {
    fn login_url(&self, redirect_uri: &str, state: &str) -> Result<String, ProviderError>;
    async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<TokenPair, ProviderError>;
    async fn refresh_token(&self, refresh_token: &str) -> Result<TokenPair, ProviderError>;
    async fn get_user(&self, access_token: &str) -> Result<UserInfo, ProviderError>;
    async fn get_user_orgs(&self, access_token: &str) -> Result<Vec<OrgInfo>, ProviderError>;
}
```

### Shared types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Github,
    Gitlab,
    Gitea,  // covers Forgejo + Codeberg
}

pub struct Connection {
    pub platform: Platform,
    pub base_url: String,
    pub credentials: Credentials,
    pub repo: RepoCoord,
}

pub enum Credentials {
    GithubInstallation { installation_id: i64 },
    ServiceToken(SecretString),
}

pub struct RepoCoord {
    pub owner: String,
    pub name: String,
    pub platform_repo_id: String,  // string, not i64 — universal
}

pub struct RefTip {
    pub name: String,
    pub oid: String,
}

pub struct TokenPair {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub access_expires_at: Option<DateTime<Utc>>,
    pub refresh_expires_at: Option<DateTime<Utc>>,
}

pub struct UserInfo {
    pub platform: Platform,
    pub platform_user_id: String,
    pub login: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
}
```

### Provider registry

```rust
pub struct ProviderRegistry {
    github: Option<GithubProvider>,
    gitlab: Option<GitlabProvider>,
    gitea: Option<GiteaProvider>,
}

impl ProviderRegistry {
    pub fn git_host(&self, platform: Platform) -> &dyn GitHostClient;
    pub fn user_fetcher(&self, platform: Platform) -> &dyn UserContentFetcher;
    pub fn oauth(&self, platform: Platform) -> &dyn OAuthProvider;
}
```

Constructed once at boot. Each provider wraps a `reqwest::Client` plus
platform-specific config (GitHub App PEM, OAuth credentials, etc.).

## Workspace layout

```text
crates/
  indexer-provider/          — trait defs, shared types, Platform enum, git ls-remote helper
  indexer-provider-github/   — renamed from indexer-github, implements traits
  indexer-provider-gitlab/   — new
  indexer-provider-gitea/    — new, covers Gitea + Forgejo + Codeberg
  indexer-server/            — existing (auth routes become multi-provider)
  indexer-ingest/            — existing (reconciler + worker become generic)
  indexer-db/                — existing (migration 0006)
  indexer-search/            — existing (unchanged)
```

## Universal ref listing: `git ls-remote`

Rather than implementing per-platform REST API calls for ref listing (which
varies wildly and some platforms don't expose custom refs via their API), we
use `git ls-remote` over HTTPS for all platforms. The git smart HTTP protocol
is the one thing every git host supports identically.

```sh
git ls-remote --refs https://{auth}@{host}/{owner}/{repo}.git refs/dkod/*
```

This is a lightweight operation — it only fetches ref advertisements, no
objects. Auth is the service token (or GitHub installation token) embedded in
the HTTPS URL or passed as a header.

For blob fetching, each platform has a simple "get blob by SHA" REST endpoint:

| Platform | Blob endpoint |
|---|---|
| GitHub | `GET /repos/:owner/:repo/git/blobs/:sha` |
| GitLab | `GET /api/v4/projects/:id/repository/blobs/:sha/raw` |
| Gitea | `GET /api/v1/repos/:owner/:repo/git/blobs/:sha` |

## Database schema changes

### Strategy

Additive migration. Existing `github_*` columns become nullable; new
platform-agnostic columns are added alongside. GitHub provider continues to
populate both.

### Migration 0006: multi-platform support

**orgs:** add `platform TEXT`, `platform_org_id TEXT`, `base_url TEXT`.
Backfill from `github_org_id`. Make `github_org_id` nullable. New unique
index on `(platform, platform_org_id, COALESCE(base_url, ''))`.

**users:** add `platform TEXT`, `platform_user_id TEXT`. Backfill from
`github_user_id`. Make `github_user_id` nullable. New unique index on
`(platform, platform_user_id)`.

**connections (new table):** non-GitHub equivalent of `installations`.

```sql
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
```

**repos:** add `platform TEXT`, `platform_repo_id TEXT`, `base_url TEXT`,
`connection_id UUID` (FK to `connections`). Make `installation_id` and
`github_repo_id` nullable. New unique index on
`(platform, platform_repo_id, COALESCE(base_url, ''))`.

**user_tokens:** change PK from `(user_id)` to `(user_id, platform)`.

**Unchanged tables:** `sessions`, `ingest_jobs`, `ingest_dead_letters` —
already platform-agnostic via `repo_id` / `org_id` UUIDs.

## Per-platform implementation

### GitHub (`indexer-provider-github`)

Reorganization of existing `indexer-github`. Code is already written —
implements the new traits by wrapping existing functions.

- `GitHostClient`: wraps `GhClient::list_refs` + `GhClient::get_blob`
- `UserContentFetcher`: wraps existing blob fetch with user token
- `OAuthProvider`: wraps existing OAuth + user API code

Webhook handler simplified: keep `installation`, `installation_repositories`,
`ping`. Remove `push` handler (reconciler is the sole ingest path).

### GitLab (`indexer-provider-gitlab`)

- `GitHostClient`: `git ls-remote` for refs, `GET /api/v4/projects/:id/repository/blobs/:sha/raw` for blobs
- `OAuthProvider`: standard OAuth2 against `gitlab.com/oauth/authorize` + `/oauth/token`. Scopes: `read_api`, `read_user`, `openid`
- `UserContentFetcher`: same blob endpoint with user's OAuth bearer token
- User info: `GET /api/v4/user`; user groups: `GET /api/v4/groups?min_access_level=10`
- Self-hosted: same API v4, different `base_url`

### Gitea/Forgejo/Codeberg (`indexer-provider-gitea`)

- `GitHostClient`: `git ls-remote` for refs, `GET /api/v1/repos/:owner/:repo/git/blobs/:sha` for blobs
- `OAuthProvider`: `{host}/login/oauth/authorize` + `/login/oauth/access_token`
- `UserContentFetcher`: same blob endpoint with user's OAuth bearer token
- User info: `GET /api/v1/user`; user orgs: `GET /api/v1/user/orgs`
- Codeberg = `codeberg.org`, Forgejo/Gitea = self-hosted, all same API

## Onboarding flows

### GitHub (unchanged)

1. Org admin installs dkod GitHub App on repos
2. `installation.created` webhook seeds `installations` + `repos`
3. User signs in via GitHub OAuth

### Non-GitHub (new)

1. Admin signs in via their platform's OAuth
2. Admin clicks "Connect a git host" → picks platform
3. Admin creates a read-only token on their platform, pastes it into dkod
4. dkod validates the token, lists available repos
5. Admin selects repos to index
6. dkod creates `connections` + `repos` rows; reconciler starts polling

### Connection management API

```text
POST   /api/connections                       — create connection
GET    /api/connections                       — list connections
DELETE /api/connections/:id                   — remove connection
POST   /api/connections/:id/repos             — add repos
DELETE /api/connections/:id/repos/:id          — remove repo
POST   /api/connections/:id/validate           — test token validity
GET    /api/connections/:id/available-repos    — list repos for picker
```

## Auth routing

```text
GET /auth/login/github    → github.com OAuth
GET /auth/login/gitlab    → gitlab.com OAuth
GET /auth/login/gitea     → {instance} OAuth
GET /auth/callback/:platform → handles callback for any platform
```

Login page shows provider buttons. Each callback dispatches to the right
`OAuthProvider` impl for code exchange, then follows the existing flow.

## Privacy model

| Platform | Indexer polling token | Content fetch token | Permission enforcement |
|---|---|---|---|
| GitHub | Installation token (App) | User's `gho_*` OAuth token | GitHub repo permissions |
| GitLab | Admin's service token | User's OAuth bearer token | GitLab project membership |
| Gitea | Admin's service token | User's OAuth bearer token | Gitea repo permissions |
| Self-hosted (V1) | Admin's service token | Service token (fallback) | All connected users see all repos |

Self-hosted gets full user-token privacy in V1.5 when per-connection OAuth
app registration is implemented.

## Rate limits

| Platform | `git ls-remote` rate | Blob fetch (REST) rate | Concern at V1 scale |
|---|---|---|---|
| GitHub | 5,000 reqs/h per install | 5,000/h per user | No |
| GitLab.com | 2,000 reqs/min per token | 2,000/min per user | No |
| Gitea/Codeberg | Varies (typically no limit) | Varies | No |

## CLI impact

None. The CLI is fully platform-agnostic. It writes blobs under
`refs/dkod/sessions/*` via gitoxide and pushes via standard `git push`.
The reconciler discovers new refs — no CLI→indexer communication exists.

## What doesn't change

- `dkod-cli` — zero changes
- `dkod-app` — zero changes (local-only, reads git refs)
- `dkod-core` — zero changes (session schema, ref layout)
- `indexer-search` — zero changes
- `indexer-ingest` reaper — zero changes
- `sessions` table — zero changes (already platform-agnostic)
- `ingest_jobs` / `ingest_dead_letters` — zero changes

## Build order

**Phase 1 — Foundation:**
1. `indexer-provider` crate (traits, types, `git ls-remote` helper)
2. Migration 0006 (platform columns, `connections` table)
3. `indexer-provider-github` (rename + implement traits)
4. Reconciler + worker generic over `GitHostClient`
5. Remove `push` webhook handler
6. Checkpoint: GitHub path works through new trait layer, all tests pass

**Phase 2 — Multi-provider auth:**
7. Multi-provider OAuth routing in `indexer-server`
8. `user_tokens` keyed by `(user_id, platform)`
9. Platform-aware content proxy
10. Connection management API
11. Dashboard login page + connection UI in `dkod-web`

**Phase 3 — GitLab provider:**
12. `indexer-provider-gitlab` crate
13. Register OAuth app on gitlab.com
14. Wire into `ProviderRegistry`
15. End-to-end test

**Phase 4 — Gitea provider:**
16. `indexer-provider-gitea` crate
17. Register OAuth app on codeberg.org
18. Wire into `ProviderRegistry`
19. End-to-end test

**Phase 5 — Self-hosted:**
20. Custom instance URL in connection setup
21. Per-connection OAuth app registration
22. Test against self-hosted GitLab CE and Gitea

## Platform groupings for future providers

| Group | Platforms | Notes |
|---|---|---|
| Bitbucket | Cloud + Server/DC | Two separate impls (different APIs) |
| SourceHut | sr.ht | Unique GraphQL API |
| CodeCommit | AWS | AWS SDK, but HTTPS clone works for `ls-remote` |
| Others | Google CSR, SourceForge, Phabricator | Low priority |

All future providers follow the same pattern: implement three traits, add a
`Platform` variant, wire into the registry. Core logic never changes.
