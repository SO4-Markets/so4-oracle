#!/usr/bin/env bash
# Fail if a secret is assigned in a tracked deployment manifest (#1232).
#
# fly.toml's [env] and railway.json's "variables" are committed to git (and
# [env] is visible via `fly config show`), so the keeper signing keys and the
# admin token must be set with `fly secrets set` / Railway sealed variables,
# never written into these files. Comment lines in fly.toml are ignored so the
# file can still document the `fly secrets set` command.
set -euo pipefail

SECRET_NAMES='KEEPER_PRIVATE_KEY|KEEPER_SECRET_KEY|ADMIN_API_TOKEN|PYTH_API_KEY'
status=0

while IFS= read -r file; do
  case "$file" in
    *.toml) pattern="^[[:space:]]*(${SECRET_NAMES})[[:space:]]*=" ;;
    *.json) pattern="\"(${SECRET_NAMES})\"[[:space:]]*:" ;;
    *) continue ;;
  esac
  if hits=$(grep -nE "$pattern" "$file"); then
    echo "::error file=${file}::secret variable assigned in a tracked manifest (#1232). Use 'fly secrets set' / Railway sealed variables instead."
    echo "$hits" | sed "s|^|  ${file}:|"
    status=1
  fi
done < <(git ls-files 'fly*.toml' 'railway*.json')

if [ "$status" -eq 0 ]; then
  echo "OK: no secrets assigned in fly*.toml / railway*.json"
fi
exit "$status"
