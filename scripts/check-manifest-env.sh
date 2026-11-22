#!/usr/bin/env bash
# Fail if the deployment manifests or .env.example drift from what `Config`
# actually reads (#1442).
#
# The variable list is derived from oracle/src/config.rs (the env-var name
# literals inside `Config::from_lookup`, plus `ENV_KEY`), so adding a variable
# there without documenting it fails CI. Checks:
#   1. every required non-secret variable is set in fly.toml [env] and in
#      railway.json "variables";
#   2. every variable Config reads appears in .env.example (commented out is
#      fine for optional ones);
#   3. neither manifest sets a variable nothing reads.
# Secrets must stay out of the manifests; check-manifest-secrets.sh enforces
# that, so they are only required in .env.example here.
set -euo pipefail

CONFIG=oracle/src/config.rs
SECRETS='KEEPER_PRIVATE_KEY KEEPER_SECRET_KEY ADMIN_API_TOKEN PYTH_API_KEY'
# Read outside Config: RUST_LOG and LOG_FORMAT by init_tracing in main.rs.
READ_ELSEWHERE='RUST_LOG LOG_FORMAT'
# Accepted aliases that need not be documented separately.
ALIASES='ORACLE'

strip_cr() { tr -d '\r' < "$1"; }

from_lookup_body() {
  strip_cr "$CONFIG" | awk '/fn from_lookup\(/{f=1} f{print} f && /^    }$/{exit}'
}

config_vars=$(
  {
    from_lookup_body | grep -oE '"[A-Z][A-Z0-9_]{2,}"' | tr -d '"'
    strip_cr "$CONFIG" | sed -nE 's/^pub const ENV_KEY: &str = "([A-Z0-9_]+)";/\1/p'
  } | sort -u
)
required_vars=$(
  from_lookup_body \
    | grep -oE 'required(_any)?\(&mut lookup, "[A-Z0-9_]+"' \
    | grep -oE '"[A-Z0-9_]+"' | tr -d '"' | sort -u
)
fly_vars=$(
  strip_cr fly.toml \
    | awk '/^\[env\]/{f=1; next} /^\[/{f=0} f' \
    | sed -nE 's/^[[:space:]]*([A-Z][A-Z0-9_]*)[[:space:]]*=.*/\1/p' | sort -u
)
railway_vars=$(
  strip_cr railway.json \
    | awk '/"variables"[[:space:]]*:/{f=1; next} f && /}/{exit} f' \
    | sed -nE 's/^[[:space:]]*"([A-Z][A-Z0-9_]*)"[[:space:]]*:.*/\1/p' | sort -u
)
example_vars=$(
  strip_cr .env.example \
    | sed -nE 's/^#?[[:space:]]*([A-Z][A-Z0-9_]*)=.*/\1/p' | sort -u
)

if [ -z "$config_vars" ] || [ -z "$required_vars" ]; then
  echo "::error file=${CONFIG}::could not extract env var names from Config::from_lookup; update scripts/check-manifest-env.sh"
  exit 1
fi

has() { grep -qx "$2" <<<"$1"; }
in_list() { case " $1 " in *" $2 "*) return 0 ;; *) return 1 ;; esac; }

status=0
fail() {
  echo "::error file=$1::$2"
  status=1
}

for var in $required_vars; do
  in_list "$SECRETS" "$var" && continue
  in_list "$ALIASES" "$var" && continue
  has "$fly_vars" "$var" || fail fly.toml "required variable $var is not set in [env] (#1442)"
  has "$railway_vars" "$var" || fail railway.json "required variable $var is not set in \"variables\" (#1442)"
done

for var in $config_vars; do
  in_list "$ALIASES" "$var" && continue
  has "$example_vars" "$var" || fail .env.example "$var is read by Config but not documented here (#1442)"
done

for var in $fly_vars; do
  has "$config_vars" "$var" || in_list "$READ_ELSEWHERE" "$var" \
    || fail fly.toml "$var is set in [env] but nothing reads it (#1442)"
done
for var in $railway_vars; do
  has "$config_vars" "$var" || in_list "$READ_ELSEWHERE" "$var" \
    || fail railway.json "$var is set in \"variables\" but nothing reads it (#1442)"
done

if [ "$status" -eq 0 ]; then
  echo "OK: fly.toml, railway.json and .env.example match the $(wc -w <<<"$config_vars" | tr -d ' ') variables Config reads"
fi
exit "$status"
