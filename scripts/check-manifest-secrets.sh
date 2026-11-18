#!/usr/bin/env bash
# Fail if a secret is assigned in a tracked deployment manifest (#1232).
#
# fly.toml's [env] and railway.json's "variables" are committed to git (and
# [env] is visible via `fly config show`), so the keeper signing keys and the
# admin token must be set with `fly secrets set` / Railway sealed variables,
# never written into these files. Comment lines in fly.toml are ignored so the
# file can still document the `fly secrets set` command.
#
# Usage:
#   check-manifest-secrets.sh              check the tracked fly*.toml / railway*.json
#   check-manifest-secrets.sh --self-test  prove the check fails on planted secrets
#                                          and passes on clean manifests (#1439)
set -euo pipefail

SECRET_NAMES='KEEPER_PRIVATE_KEY|KEEPER_SECRET_KEY|ADMIN_API_TOKEN|PYTH_API_KEY'

# Returns non-zero (and prints a CI annotation) if $1 assigns a secret.
check_file() {
  local file=$1 pattern hits
  case "$file" in
    *.toml) pattern="^[[:space:]]*(${SECRET_NAMES})[[:space:]]*=" ;;
    *.json) pattern="\"(${SECRET_NAMES})\"[[:space:]]*:" ;;
    *) return 0 ;;
  esac
  if hits=$(grep -nE "$pattern" "$file"); then
    echo "::error file=${file}::secret variable assigned in a tracked manifest (#1232). Use 'fly secrets set' / Railway sealed variables instead."
    echo "$hits" | sed "s|^|  ${file}:|"
    return 1
  fi
  return 0
}

# Checks every file given; non-zero if any of them assigns a secret.
scan() {
  local status=0 file
  for file in "$@"; do
    check_file "$file" || status=1
  done
  return "$status"
}

self_test() {
  local failed=0
  # Global, not local: the EXIT trap runs after this function returns.
  SELF_TEST_DIR=$(mktemp -d)
  local dir=$SELF_TEST_DIR
  trap 'rm -rf "$SELF_TEST_DIR"' EXIT

  printf '[env]\n  KEEPER_SECRET_KEY = "SPLANTED"\n' >"$dir/fly.toml"
  printf '{"variables": {"ADMIN_API_TOKEN": "planted"}}\n' >"$dir/railway.json"
  printf '# fly secrets set KEEPER_SECRET_KEY=...\n# KEEPER_SECRET_KEY = "in a comment"\n[env]\n  STELLAR_NETWORK = "testnet"\n' >"$dir/fly.clean.toml"
  printf '{"variables": {"STELLAR_NETWORK": "testnet"}}\n' >"$dir/railway.clean.json"

  for file in fly.toml railway.json; do
    if scan "$dir/$file" >/dev/null; then
      echo "self-test FAILED: planted secret in ${file} was not detected"
      failed=1
    fi
  done
  for file in fly.clean.toml railway.clean.json; do
    if ! scan "$dir/$file" >/dev/null; then
      echo "self-test FAILED: clean ${file} was flagged"
      failed=1
    fi
  done

  if [ "$failed" -eq 0 ]; then
    echo "OK: self-test detected planted secrets and passed clean manifests"
  fi
  return "$failed"
}

if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit $?
fi

files=()
while IFS= read -r file; do
  files+=("$file")
done < <(git ls-files 'fly*.toml' 'railway*.json')

if scan "${files[@]+"${files[@]}"}"; then
  echo "OK: no secrets assigned in fly*.toml / railway*.json"
  exit 0
fi
exit 1
