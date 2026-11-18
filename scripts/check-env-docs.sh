#!/usr/bin/env bash
# Fail if docs/ENVIRONMENT.md and the variables the code reads disagree (#1432).
#
# The code side is every quoted UPPER_SNAKE name inside Config::from_lookup in
# oracle/src/config.rs, the PRICE_FEED_CONFIG key (`ENV_KEY`), and every
# `std::env::var("…")` in oracle/src/main.rs. The docs side is every table row
# in docs/ENVIRONMENT.md whose first cell is a backticked name. Both directions
# are checked, so a new variable must be documented and a removed one deleted.
set -euo pipefail

DOC=docs/ENVIRONMENT.md
CONFIG=oracle/src/config.rs
MAIN=oracle/src/main.rs

code_vars() {
  awk '/fn from_lookup/ { inside = 1 } inside { print } inside && /^    }$/ { exit }' "$CONFIG" \
    | grep -oE '"[A-Z][A-Z0-9_]{2,}"'
  grep -oE 'ENV_KEY: &str = "[A-Z][A-Z0-9_]+"' "$CONFIG" | grep -oE '"[A-Z][A-Z0-9_]+"'
  grep -oE 'env::var\("[A-Z][A-Z0-9_]+"\)' "$MAIN" | grep -oE '"[A-Z][A-Z0-9_]+"'
}

doc_vars() {
  grep -oE '^\| `[A-Z][A-Z0-9_]+`' "$DOC" | grep -oE '[A-Z][A-Z0-9_]+'
}

code=$(code_vars | tr -d '"' | sort -u)
docs=$(doc_vars | sort -u)

if [ -z "$code" ]; then
  echo "::error file=${CONFIG}::found no environment variables in the code; this check's patterns need updating"
  exit 1
fi

status=0
while IFS= read -r name; do
  echo "::error file=${DOC}::${name} is read by the code but not documented in ${DOC}"
  status=1
done < <(comm -23 <(echo "$code") <(echo "$docs"))
while IFS= read -r name; do
  echo "::error file=${DOC}::${name} is documented in ${DOC} but no longer read by the code"
  status=1
done < <(comm -13 <(echo "$code") <(echo "$docs"))

if [ "$status" -eq 0 ]; then
  echo "OK: ${DOC} matches the $(echo "$code" | wc -l | tr -d ' ') environment variables the code reads"
fi
exit "$status"
