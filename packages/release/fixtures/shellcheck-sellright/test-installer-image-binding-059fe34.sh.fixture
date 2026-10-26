#!/bin/sh
# Offline command-stub regressions: no Docker daemon, registry, or host install.
set -eu
unset CDPATH
DEPLOY="$(cd -- "$(dirname -- "$0")/.." && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' 0
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'ok - %s\n' "$*"; }
command -v jq >/dev/null 2>&1 || fail 'jq required'
REAL_JQ=$(command -v jq)
export REAL_JQ
mkdir -p "$tmp/bin" "$tmp/home" "$tmp/source/deploy" "$tmp/source/packages/admin"
printf 'SELLRIGHT_IMAGE_PREFIX=ghcr.io/test/preserved\nSELLRIGHT_IMAGE_TAG=preserved\n' > "$tmp/home/.env"
printf 'services: {}\n' > "$tmp/home/compose.yaml"
cp "$DEPLOY/compose.build.yaml" "$tmp/source/deploy/compose.build.yaml"
for script in install sellright; do
  grep -q '^main "\$@"$' "$DEPLOY/$script.sh" || fail 'entrypoint moved'
  sed '$ { /^main "\$@"$/d; }' "$DEPLOY/$script.sh" > "$tmp/$script-functions.sh"
done
cat > "$tmp/bin/docker" <<'SH'
#!/bin/sh
set -eu
printf 'docker %s\n' "$*" >> "$TRACE"
if [ "$1" = pull ]; then
  case "$2" in ghcr.io/test/preserved-*:preserved) exit 0 ;; *) exit 91 ;; esac
fi
if [ "$1" = inspect ]; then
  for arg in "$@"; do container=$arg; done
  case "$container" in
    "$(printf '%064d' 7)") image=8 ;;
    "$(printf '%064d' 8)") [ "$NO_ROLLBACK_ADMIN" = 0 ] || exit 104; image=9 ;;
    "$(printf '%064d' 9)") image=10 ;;
    *) exit 104 ;;
  esac
  # Local-built images deliberately have no RepoDigests query path.
  printf 'sha256:%064d\n' "$image"
  exit 0
fi
if [ "$1" = exec ]; then
  [ "$2" = "$(printf '%064d' 7)" ] && [ "$3" = tar ] || exit 105
  printf 'archive-bytes'
  exit 0
fi
if [ "$1" = image ] && [ "$2" = inspect ]; then
  for arg in "$@"; do ref=$arg; done
  [ "${BAD_DIGEST:-0}" = 0 ] || { printf 'not-a-digest\n'; exit 0; }
  case "$ref" in
    *preserved-api:preserved) svc=api; n=1; [ ! -e "$SWAPPED" ] || n=9 ;;
    *preserved-admin:preserved) svc=admin; n=2 ;;
    *preserved-storefront:preserved) svc=storefront; n=3 ;;
    *) exit 92 ;;
  esac
  if [ "${REPO_MISMATCH:-0}" = 1 ]; then
    printf '["ghcr.io/test/other@sha256:%064d"]\n' 9
  else
    printf '["ghcr.io/test/other@sha256:%064d","ghcr.io/test/preserved-%s@sha256:%064d"]\n' 9 "$svc" "$n"
  fi
  exit 0
fi
[ "$1" = compose ] || exit 93
shift
override=''
project_directory=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --env-file) shift 2 ;;
    --project-directory) project_directory=$2; shift 2 ;;
    -f) override=$2; shift 2 ;;
    *) break ;;
  esac
done
case "$1" in
  ps)
    for arg in "$@"; do svc=$arg; done
    case "$svc" in
      api) [ "$NO_RUNNING_API" = 0 ] || exit 0; container=7 ;;
      admin) container=8 ;;
      storefront) container=9 ;;
      *) exit 108 ;;
    esac
    printf '%064d\n' "$container"
    exit 0
    ;;
  exec)
    case " $* " in *' postgres pg_dump '*) printf 'database-bytes' ;; *) exit 106 ;; esac
    ;;
  config)
    if [ "$RECOVERY_DATA" = 1 ]; then
      "$REAL_JQ" -n --arg id "$LITERAL_ID" --arg key "$LITERAL_KEY" '{services:{api:{environment:{RECOVERY_KIT_ID:$id,SELLRIGHT_MASTER_KEY:$key}}}}'
      exit 0
    fi
    if [ "${MISSING_REF:-0}" = 1 ]; then
      printf '{"services":{"api":{}}}\n'
      exit 0
    fi
    if [ "${SOURCE_CONFIG:-0}" = 1 ]; then
      printf '{"services":{"api":{"image":"ghcr.io/test/preserved-api:preserved","build":{"context":"."}}}}\n'
      exit 0
    fi
    if [ "${BUILD_POLICY:-0}" = 1 ]; then
      printf '{"services":{"api":{"image":"ghcr.io/test/preserved-api:preserved","pull_policy":"build"}}}\n'
      exit 0
    fi
    cat <<'JSON'
{"services":{"postgres":{"image":"postgres:17"},"api":{"image":"ghcr.io/test/preserved-api:preserved"},"admin":{"image":"ghcr.io/test/preserved-admin:preserved"},"storefront":{"image":"ghcr.io/test/preserved-storefront:preserved"}}}
JSON
    ;;
  pull) [ "$MODE" = registry ] || exit 94 ;;
  build)
    [ "$MODE" = source ] || exit 95
    [ "$override" = "$SELLRIGHT_SOURCE/deploy/compose.build.yaml" ] || exit 109
    [ "$project_directory" = "$SELLRIGHT_SOURCE/deploy" ] || exit 110
    n=0
    for context in $(awk '$1 == "context:" { print $2 }' "$override"); do
      n=$((n + 1))
      resolved=$(cd "$project_directory/$context" && pwd -P)
      case "$n" in 1|3) expected=$SELLRIGHT_SOURCE ;; 2) expected=$SELLRIGHT_SOURCE/packages/admin ;; *) exit 111 ;; esac
      [ "$resolved" = "$expected" ] || exit 112
    done
    [ "$n" -eq 3 ] || exit 113
    printf 'build-contexts chosen-checkout\n' >> "$TRACE"
    ;;
  run|up)
    action=$1
    case " $* " in *' --pull never '*) : ;; *) exit 96 ;; esac
    if [ "$action" = up ]; then
      case " $* " in *' --no-build '*) : ;; *) exit 97 ;; esac
    fi
    case " $* " in
      *' --no-deps '*)
        if [ "$action" = up ]; then
          n=7
          for svc in api admin storefront; do
            n=$((n + 1))
            image=$(awk -v svc="$svc:" '$1 == svc { getline; print $2 }' "$override")
            [ "$image" = "$(printf 'sha256:%064d' "$n")" ] || exit 114
          done
          printf 'executed-up previous-local-image-ids\n' >> "$TRACE"
          exit 0
        fi
        ;;
    esac
    if [ "$MODE" = registry ]; then
      [ -n "$override" ] && [ -s "$override" ] || exit 98
      n=0
      for svc in api admin storefront; do
        n=$((n + 1))
        digest=$(awk -v svc="$svc:" '$1 == svc { getline; print $2 }' "$override")
        expected=$(printf 'ghcr.io/test/preserved-%s@sha256:%064d' "$svc" "$n")
        [ "$digest" = "$expected" ] || exit 99
        grep -Fx "cosign $expected" "$TRACE" >/dev/null || exit 100
      done
      printf 'executed-%s verified-digests\n' "$action" >> "$TRACE"
    else
      [ "$override" = "$SELLRIGHT_HOME/compose.yaml" ] || exit 101
      printf 'executed-%s source-images\n' "$action" >> "$TRACE"
    fi
    ;;
  *) exit 102 ;;
esac
SH
cat > "$tmp/bin/cosign" <<'SH'
#!/bin/sh
set -eu
[ "$1" = verify ] || exit 103
for arg in "$@"; do digest=$arg; done
printf 'cosign %s\n' "$digest" >> "$TRACE"
case "$digest" in ghcr.io/test/other@*) exit 107 ;; esac
[ "${UNSIGNED:-0}" = 0 ] || exit 1
# Mutable api tag now points elsewhere; previously verified digest must execute.
: > "$SWAPPED"
SH
cat > "$tmp/bin/jq" <<'SH'
#!/bin/sh
set -eu
if [ "${RECOVERY_JQ_GUARD:-0}" = 1 ]; then
  if [ -n "$LITERAL_KEY" ]; then
    for arg in "$@"; do
      case "$arg" in *"$LITERAL_KEY"*) echo 'recovery key exposed in argv' >&2; exit 115 ;; esac
    done
  fi
  printf 'recovery-jq safe-argv\n' >> "$TRACE"
fi
exec "$REAL_JQ" "$@"
SH
chmod +x "$tmp/bin/docker" "$tmp/bin/cosign" "$tmp/bin/jq"
export PATH="$tmp/bin:$PATH" SELLRIGHT_HOME="$tmp/home" SELLRIGHT_SOURCE="$tmp/source"
export TRACE="$tmp/trace" SWAPPED="$tmp/swapped" CHECK_MARKER="$tmp/check-failed" RECOVERY_DATA=0 RECOVERY_JQ_GUARD=0 NO_ROLLBACK_ADMIN=0 FIRST_CHECK_FAIL=0 MODE=registry UNSIGNED=0 BAD_DIGEST=0 NO_RUNNING_API=0 MISSING_REF=0 REPO_MISMATCH=0 SOURCE_CONFIG=0 BUILD_POLICY=0
reset_case() { : > "$TRACE"; rm -f "$SWAPPED" "$CHECK_MARKER"; }
installer() (
  # shellcheck disable=SC1091
  . "$tmp/install-functions.sh"
  # shellcheck disable=SC2034 # installer defaults must not select verified refs
  IMAGE_PREFIX=ghcr.io/wrong/default
  # shellcheck disable=SC2034
  IMAGE_TAG=wrong
  verify_and_pull
  start_stack
)
update() (
  # shellcheck disable=SC1091
  . "$tmp/sellright-functions.sh"
  # Called indirectly by cmd_update() from the sourced CLI body.
  # shellcheck disable=SC2329
  cmd_maintenance() { :; }
  # Called indirectly by cmd_update() from the sourced CLI body.
  # shellcheck disable=SC2329
  cmd_functional_check() {
    if [ "$FIRST_CHECK_FAIL" = 1 ] && [ ! -f "$CHECK_MARKER" ]; then
      : > "$CHECK_MARKER"
      return 1
    fi
    printf 'functional-check healthy\n' >> "$TRACE"
  }
  cmd_update "$@"
)
assert_no_execution() {
  if grep -Eq '^docker .* (run|up) ' "$TRACE"; then fail 'unverified image reached migration/start'; fi
}
reset_case
installer > "$tmp/installer.out" 2>&1 || { cat "$tmp/installer.out" >&2; fail 'installer binding failed'; }
grep -Fx 'docker pull ghcr.io/test/preserved-api:preserved' "$TRACE" >/dev/null || fail 'preserved .env image not pulled'
if grep -q 'ghcr.io/wrong/default' "$TRACE"; then fail 'installer verified shell defaults'; fi
grep -Fx 'executed-up verified-digests' "$TRACE" >/dev/null || fail 'installer did not start verified digests'
pass 'installer uses configured images & frozen verified digest despite tag swap'

reset_case
UNSIGNED=1
if installer > "$tmp/unsigned-install.out" 2>&1; then fail 'unsigned installer succeeded'; fi
assert_no_execution
pass 'unsigned installer never starts services'

reset_case
UNSIGNED=0 BAD_DIGEST=1
if installer > "$tmp/invalid-install.out" 2>&1; then fail 'invalid digest installer succeeded'; fi
assert_no_execution
pass 'invalid digest installer fails closed'

reset_case
BAD_DIGEST=0
update > "$tmp/update.out" 2>&1 || { cat "$tmp/update.out" >&2; fail 'update binding failed'; }
grep -Fx 'executed-run verified-digests' "$TRACE" >/dev/null || fail 'migration did not use verified digests'
grep -Fx 'executed-up verified-digests' "$TRACE" >/dev/null || fail 'update did not use verified digests'
existing=$(printf '%064d' 7)
grep -Fx "docker exec $existing tar czf - -C /app/var assets" "$TRACE" >/dev/null || fail 'backup did not use existing API container'
grep -Fx "docker exec $existing tar czf - -C /app/var downloads" "$TRACE" >/dev/null || fail 'downloads backup did not use existing API container'
awk '/^docker exec / { backup=NR } /^cosign / && !gate { gate=NR } END { exit !(backup && gate && backup < gate) }' "$TRACE" || fail 'backup did not precede verification'
pass 'registry update backs up existing bytes, then migrates & starts verified digests'

reset_case
NO_RUNNING_API=1
if update > "$tmp/missing-api.out" 2>&1; then fail 'missing existing API backup succeeded'; fi
assert_no_execution
if grep -Eq '^docker exec |^cosign ' "$TRACE"; then fail 'missing API did not stop before backup/verification'; fi
NO_RUNNING_API=0
pass 'missing existing API fails closed before backup execution'

reset_case
UNSIGNED=1
if update > "$tmp/unsigned-update.out" 2>&1; then fail 'unsigned update succeeded'; fi
assert_no_execution
pass 'unsigned update never migrates or starts'

reset_case
UNSIGNED=0 BAD_DIGEST=1
if update > "$tmp/invalid-update.out" 2>&1; then fail 'invalid digest update succeeded'; fi
assert_no_execution
pass 'invalid digest update never migrates or starts'

reset_case
BAD_DIGEST=0 MISSING_REF=1
if installer > "$tmp/missing-ref-install.out" 2>&1; then fail 'missing configured image installer succeeded'; fi
assert_no_execution
reset_case
if update > "$tmp/missing-ref-update.out" 2>&1; then fail 'missing configured image update succeeded'; fi
assert_no_execution
MISSING_REF=0
pass 'missing configured application image fails closed'

for rejection in REPO_MISMATCH SOURCE_CONFIG BUILD_POLICY; do
  reset_case
  export "$rejection=1"
  if installer > "$tmp/rejection-install.out" 2>&1; then fail "installer accepted $rejection"; fi
  assert_no_execution
  reset_case
  if update > "$tmp/rejection-update.out" 2>&1; then fail "update accepted $rejection"; fi
  assert_no_execution
  export "$rejection=0"
done
pass 'registry paths reject other repositories & configured source builds'

reset_case
BAD_DIGEST=0 MODE=source
update --from-source > "$tmp/source.out" 2>&1 || { cat "$tmp/source.out" >&2; fail 'explicit source update failed'; }
if grep -Eq '^cosign |^docker pull |^docker .* pull$' "$TRACE"; then fail 'source update used registry verification/pull'; fi
grep -Fx 'executed-run source-images' "$TRACE" >/dev/null || fail 'source migration missing'
grep -Fx 'executed-up source-images' "$TRACE" >/dev/null || fail 'source startup missing'
grep -Fx 'build-contexts chosen-checkout' "$TRACE" >/dev/null || fail 'build contexts did not resolve against chosen checkout'
pass 'explicit source update builds actual checkout contexts & skips registry gate'

reset_case
FIRST_CHECK_FAIL=1
if update --from-source > "$tmp/source-rollback.out" 2>&1; then fail 'failed source functional check reported update success'; fi
grep -Fx 'executed-up previous-local-image-ids' "$TRACE" >/dev/null || fail 'source rollback did not restore all existing local image IDs'
grep -Fx 'functional-check healthy' "$TRACE" >/dev/null || fail 'restored images not checked'
awk '/^docker inspect / { captured++ } /build-contexts/ { exit !(captured == 4) }' "$TRACE" || fail 'all rollback identities were not captured before build'
if grep -q 'RepoDigests' "$TRACE"; then fail 'local-image rollback required published digests'; fi
pass 'failed source check restores complete immutable local-image plan without pulling/building'
FIRST_CHECK_FAIL=0

reset_case
NO_ROLLBACK_ADMIN=1
if update --from-source > "$tmp/incomplete-rollback.out" 2>&1; then fail 'incomplete rollback plan accepted'; fi
assert_no_execution
if grep -q 'build-contexts' "$TRACE"; then fail 'build began before complete rollback capture'; fi
NO_ROLLBACK_ADMIN=0
pass 'incomplete previous image plan fails closed before rebuild'

reset_case
RECOVERY_DATA=1 RECOVERY_JQ_GUARD=1
export LITERAL_ID='literal "kit" id'
# This literal shell payload must stay unexpanded until consumed as data.
# shellcheck disable=SC2016
LITERAL_KEY=$(printf 'literal $(touch %s/env-substitution)' "$tmp")
export LITERAL_KEY
# Valid shell syntax would execute both markers if a regression sourced it.
printf "RECOVERY_KIT_ID='%s'\nSELLRIGHT_MASTER_KEY=\"%s\"\ntouch \"%s\"\n" "$LITERAL_ID" "$LITERAL_KEY" "$tmp/env-executed" > "$SELLRIGHT_HOME/.env"
(
  # shellcheck disable=SC1091
  . "$tmp/install-functions.sh"
  write_recovery_kit
) > "$tmp/recovery.out" 2>&1 || { cat "$tmp/recovery.out" >&2; fail 'recovery data parsing failed'; }
[ ! -e "$tmp/env-executed" ] && [ ! -e "$tmp/env-substitution" ] || fail 'preserved .env executed shell syntax'
# Variables in this literal filter belong to jq, not the shell.
# shellcheck disable=SC2016
"$REAL_JQ" -e --arg id "$LITERAL_ID" --arg key "$LITERAL_KEY" '.kitId == $id and .masterKey == $key' "$SELLRIGHT_HOME/recovery-kit.json" >/dev/null || fail 'recovery kit did not retain literal resolved values'
grep -Fx 'recovery-jq safe-argv' "$TRACE" >/dev/null || fail 'recovery argv guard not exercised'
pass 'recovery kit consumes literal Compose values via stdin without shell execution or secret argv'

rm "$SELLRIGHT_HOME/recovery-kit.json"
LITERAL_KEY=''
if (
  # shellcheck disable=SC1091
  . "$tmp/install-functions.sh"
  write_recovery_kit
) > "$tmp/invalid-recovery.out" 2>&1; then fail 'empty recovery key accepted'; fi
[ ! -e "$SELLRIGHT_HOME/recovery-kit.json" ] || fail 'failed recovery write left an accepted kit'
pass 'invalid recovery key fails closed without leaving a kit'
printf '\nall installer/update image-binding tests passed\n'
