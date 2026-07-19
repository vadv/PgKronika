#!/bin/sh
set -eu

fail() {
  printf 'BDD runtime contract failed: %s\n' "$*" >&2
  exit 1
}

: "${KRONIKA_PG_MATRIX:?KRONIKA_PG_MATRIX is required}"

old_ifs=$IFS
IFS=';'
set -- $KRONIKA_PG_MATRIX
IFS=$old_ifs
seen=''

for entry do
  major=${entry%%=*}
  bin=${entry#*=}
  [ "$major" != "$entry" ] || fail "invalid matrix entry: $entry"
  case " 15 16 17 18 " in
    *" $major "*) ;;
    *) fail "unexpected PostgreSQL major: $major" ;;
  esac
  case " $seen " in
    *" $major "*) fail "duplicate PostgreSQL major: $major" ;;
  esac
  seen="$seen $major"

  root=${bin%/bin}
  [ "$root" != "$bin" ] || fail "PG$major bin path must end in /bin"
  [ -x "$bin/postgres" ] || fail "PG$major postgres is not executable"
  [ -f "$root/lib/pg_store_plans.so" ] || fail "PG$major pg_store_plans.so is missing"

  extension_dir="$root/share/postgresql/extension"
  [ -f "$extension_dir/pg_store_plans.control" ] || fail "PG$major control file is missing"
  sql_found=false
  for sql in "$extension_dir"/pg_store_plans--*.sql; do
    if [ -f "$sql" ]; then
      sql_found=true
      break
    fi
  done
  [ "$sql_found" = true ] || fail "PG$major extension SQL is missing"
done

for major in 15 16 17 18; do
  case " $seen " in
    *" $major "*) ;;
    *) fail "PG$major matrix entry is missing" ;;
  esac
done

printf 'BDD runtime contract: PG15-18 ok\n'
