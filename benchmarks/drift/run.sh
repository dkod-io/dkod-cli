#!/usr/bin/env bash
# Drift precision benchmark.
#
# Replays every labeled session fixture in benchmarks/drift/dataset/ through
# the real `dkod drift` binary (default DriftConfig, no config file) and
# prints a confusion matrix with precision / recall / accuracy plus per-rule
# false-positive attribution. Positive class = "drift".
#
# Each fixture is exercised exactly the way production sessions are stored:
# the session JSON becomes a git blob pinned at refs/dkod/sessions/<id> in a
# fresh temp repo, mirroring dkod_core::store::write_session.
#
# Usage:
#   benchmarks/drift/run.sh            # builds release binary, runs benchmark
#   DKOD_BIN=path/to/dkod benchmarks/drift/run.sh   # skip the build
#   DKOD_DRIFT_CONFIG=path/to/config.toml benchmarks/drift/run.sh
#       # copy the given .dkod/config.toml into every fixture repo, to measure
#       # candidate DriftConfig tunings against the same dataset
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DATASET="$ROOT/benchmarks/drift/dataset"

command -v jq >/dev/null 2>&1 || { echo "error: jq is required" >&2; exit 1; }

if [[ -z "${DKOD_BIN:-}" ]]; then
  echo "building dkod (cargo build --release -p dkod-cli)..." >&2
  (cd "$ROOT" && cargo build --release -p dkod-cli --quiet)
  DKOD_BIN="$ROOT/target/release/dkod"
fi
[[ -x "$DKOD_BIN" ]] || { echo "error: $DKOD_BIN is not executable" >&2; exit 1; }
# The binary runs from inside each temp repo — make the path absolute.
case "$DKOD_BIN" in /*) ;; *) DKOD_BIN="$(cd "$(dirname "$DKOD_BIN")" && pwd)/$(basename "$DKOD_BIN")" ;; esac

tp=0 fp=0 tn=0 fn=0
fp_sensitive=0 fp_magnitude=0 fp_unmentioned=0
tp_sensitive=0 tp_magnitude=0 tp_unmentioned=0
declare -a misses=()
# per-category tallies (category = fixture filename prefix: clean / drift / hard)
declare -A cat_n cat_fp cat_fn

printf '%-9s  %-6s  %-7s  %-34s  %s\n' fixture label verdict rules result
printf '%s\n' '---------------------------------------------------------------------------'

for f in "$DATASET"/*.json; do
  name="$(basename "$f" .json)"
  cat="${name%%-*}"
  label="$(jq -r .label "$f")"
  id="$(jq -r .session.id "$f")"

  repo="$(mktemp -d)"
  git -C "$repo" init -q
  blob="$(jq -c .session "$f" | git -C "$repo" hash-object -w --stdin)"
  git -C "$repo" update-ref "refs/dkod/sessions/$id" "$blob"
  if [[ -n "${DKOD_DRIFT_CONFIG:-}" ]]; then
    mkdir -p "$repo/.dkod"
    cp "$DKOD_DRIFT_CONFIG" "$repo/.dkod/config.toml"
  fi

  # stderr silenced: dkod prints unrelated machine-state notices (e.g. agent
  # hook drift on the host) there; a genuine failure still aborts via set -e.
  out="$(cd "$repo" && "$DKOD_BIN" drift "$id" 2>/dev/null)"
  rm -rf "$repo"

  rules=""
  if grep -q 'touched sensitive path' <<<"$out"; then rules+="sensitive_path,"; fi
  if grep -q 'small-sounding request' <<<"$out"; then rules+="magnitude,"; fi
  if grep -q 'prompt referenced' <<<"$out"; then rules+="unmentioned,"; fi
  rules="${rules%,}"
  if [[ -n "$rules" ]]; then verdict="drift"; else verdict="clean"; fi

  cat_n[$cat]=$(( ${cat_n[$cat]:-0} + 1 ))
  if [[ "$label" == "drift" && "$verdict" == "drift" ]]; then
    result="TP"; tp=$((tp + 1))
    [[ "$rules" == *sensitive_path* ]] && tp_sensitive=$((tp_sensitive + 1))
    [[ "$rules" == *magnitude* ]] && tp_magnitude=$((tp_magnitude + 1))
    [[ "$rules" == *unmentioned* ]] && tp_unmentioned=$((tp_unmentioned + 1))
  elif [[ "$label" == "clean" && "$verdict" == "drift" ]]; then
    result="FP"; fp=$((fp + 1))
    cat_fp[$cat]=$(( ${cat_fp[$cat]:-0} + 1 ))
    [[ "$rules" == *sensitive_path* ]] && fp_sensitive=$((fp_sensitive + 1))
    [[ "$rules" == *magnitude* ]] && fp_magnitude=$((fp_magnitude + 1))
    [[ "$rules" == *unmentioned* ]] && fp_unmentioned=$((fp_unmentioned + 1))
    misses+=("FP  $name  [$rules]  $(jq -r .why "$f")")
  elif [[ "$label" == "drift" && "$verdict" == "clean" ]]; then
    result="FN"; fn=$((fn + 1))
    cat_fn[$cat]=$(( ${cat_fn[$cat]:-0} + 1 ))
    misses+=("FN  $name  $(jq -r .why "$f")")
  else
    result="TN"; tn=$((tn + 1))
  fi

  printf '%-9s  %-6s  %-7s  %-34s  %s\n' "$name" "$label" "$verdict" "${rules:--}" "$result"
done

total=$((tp + fp + tn + fn))
echo
echo "confusion matrix (positive class = drift)"
echo "  TP=$tp  FP=$fp  TN=$tn  FN=$fn  (n=$total)"
awk -v tp="$tp" -v fp="$fp" -v tn="$tn" -v fn="$fn" 'BEGIN {
  prec = (tp + fp) ? tp / (tp + fp) : 0
  rec  = (tp + fn) ? tp / (tp + fn) : 0
  acc  = (tp + tn) / (tp + fp + tn + fn)
  printf "  precision=%.1f%%  recall=%.1f%%  accuracy=%.1f%%\n", prec*100, rec*100, acc*100
}'
echo
echo "rule attribution (a session can fire several rules)"
echo "  sensitive_path: TP=$tp_sensitive  FP=$fp_sensitive"
echo "  magnitude:      TP=$tp_magnitude  FP=$fp_magnitude"
echo "  unmentioned:    TP=$tp_unmentioned  FP=$fp_unmentioned"
echo
echo "per-category"
for cat in clean drift hard; do
  echo "  $cat: n=${cat_n[$cat]:-0}  FP=${cat_fp[$cat]:-0}  FN=${cat_fn[$cat]:-0}"
done

if (( ${#misses[@]} > 0 )); then
  echo
  echo "misclassifications (vs human label)"
  for m in "${misses[@]}"; do echo "  $m"; done
fi
