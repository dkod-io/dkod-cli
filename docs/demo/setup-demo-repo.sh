#!/usr/bin/env bash
# Build a small throwaway demo repo for the dkod blame / drift card demo.
#
# Creates a temp git repo with:
#   * a human commit (initial app)
#   * an AI commit attributed to a clean captured session ("add logging")
#   * an AI commit attributed to a DRIFTING captured session
#     (asked: "fix the typo in the readme" — but it also rewrote the deploy
#     workflow, auth, db, and routes)
#
# Sessions are seeded exactly the way dkod persists them: the session JSON is
# written as a git blob (`git hash-object -w`) and pinned by
# `refs/dkod/sessions/<id>`; each produced commit gets a
# `refs/dkod/commits/<sha>` ref pointing at the same blob. No dkod binary is
# needed to seed; any dkod (release or target/debug) can then read the repo.
#
# Prints the demo repo path as the LAST line of output. The drifting session id
# is fixed so demos can reference it: see DRIFT_SESSION_ID below.

set -euo pipefail

CLEAN_SESSION_ID="0196f8e2-1111-7000-8000-0000000000a1"
DRIFT_SESSION_ID="0196f8e2-2222-7000-8000-0000000000d2"

DEMO_DIR="$(mktemp -d "${TMPDIR:-/tmp}/dkod-demo.XXXXXX")"
cd "$DEMO_DIR"

git init -q
git config user.name "demo"
git config user.email "demo@example.com"

mkdir -p src .github/workflows

# --- human commit: initial app -------------------------------------------
# Lines are kept short so `dkod blame` output fits an 80-column recording.
cat > src/server.js <<'EOF'
import http from "node:http";

const app = http.createServer((req, res) => {
  res.writeHead(200);
  res.end("ok");
});

app.listen(3000);
EOF
cat > README.md <<'EOF'
# demo-api

Tiny demo service used by the dkod demo GIF.
EOF
git add .
git commit -qm "initial server"

# --- AI commit 1: clean session (request logging) -------------------------
cat > src/server.js <<'EOF'
import http from "node:http";

function logRequest(req) {
  console.log(req.method, req.url);
}

const app = http.createServer((req, res) => {
  logRequest(req);
  res.writeHead(200);
  res.end("ok");
});

app.listen(3000);
EOF
git add .
git commit -qm "add request logging"
CLEAN_SHA="$(git rev-parse HEAD)"

# --- AI commit 2: drifting session ----------------------------------------
# Asked for a readme typo fix; also rewrote CI deploy, auth, db, and routes.
sed -i.bak 's/Tiny demo service/Tiny demo service./' README.md && rm README.md.bak
cat > .github/workflows/deploy.yml <<'EOF'
name: deploy
on:
  push:
    branches: [main]
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: ./scripts/deploy.sh --env prod --force
EOF
cat > src/auth.js <<'EOF'
export function verifyToken(token) {
  // TODO(agent): replaced signature validation with a length check
  return typeof token === "string" && token.length > 8;
}
EOF
cat > src/db.js <<'EOF'
export const pool = { max: 50, idleTimeoutMillis: 0 };
export function query(sql) {
  return Promise.resolve({ sql, rows: [] });
}
EOF
cat > src/routes.js <<'EOF'
import { verifyToken } from "./auth.js";
export function routes(req) {
  if (!verifyToken(req.headers.authorization)) return 401;
  return 200;
}
EOF
git add .
git commit -qm "fix readme typo"
DRIFT_SHA="$(git rev-parse HEAD)"

# --- seed the captured sessions (blob + refs, the dkod wire format) --------
seed_session() { # $1 = session id, $2 = session JSON
  local blob
  blob="$(printf '%s' "$2" | git hash-object -w --stdin)"
  git update-ref "refs/dkod/sessions/$1" "$blob"
}

link_commit() { # $1 = commit sha, $2 = session id
  git update-ref "refs/dkod/commits/$1" "$(git rev-parse "refs/dkod/sessions/$2")"
}

seed_session "$CLEAN_SESSION_ID" "$(cat <<EOF
{"id":"$CLEAN_SESSION_ID","agent":"claude_code","created_at":1765360000,"duration_ms":424000,"prompt_summary":"add logging","messages":[{"role":"user","content":"add logging"}],"commits":["$CLEAN_SHA"],"files_touched":["src/server.js"],"redaction_count":0}
EOF
)"
link_commit "$CLEAN_SHA" "$CLEAN_SESSION_ID"

seed_session "$DRIFT_SESSION_ID" "$(cat <<EOF
{"id":"$DRIFT_SESSION_ID","agent":"claude_code","created_at":1765363600,"duration_ms":917000,"prompt_summary":"fix the typo in the readme","messages":[{"role":"user","content":"fix the typo in the readme"}],"commits":["$DRIFT_SHA"],"files_touched":["README.md",".github/workflows/deploy.yml","src/auth.js","src/db.js","src/routes.js"],"redaction_count":0}
EOF
)"
link_commit "$DRIFT_SHA" "$DRIFT_SESSION_ID"

echo "seeded sessions:" >&2
echo "  clean: $CLEAN_SESSION_ID -> $CLEAN_SHA" >&2
echo "  drift: $DRIFT_SESSION_ID -> $DRIFT_SHA" >&2

# Repo path is the LAST line on stdout (consumed by the vhs tape).
echo "$DEMO_DIR"
