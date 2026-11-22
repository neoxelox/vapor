#!/bin/zsh
# wl_npm: real npm installs and removals in a project inside the sync
# folder, from an offline cache. Each step prints how long npm took and
# how long Vapor needed to go quiet after it.
#
#   wl_npm.sh setup                 unpack the cache, create ~/Vapor/webapp
#   wl_npm.sh ci                    npm ci (a clean install from the lockfile)
#   wl_npm.sh rm                    rm -rf node_modules
#   wl_npm.sh uninstall <pkg...>    npm uninstall
#   wl_npm.sh install <pkg...>      npm install
#   wl_npm.sh cycle <n> [nowait]    n times: ci, then rm; with `nowait` the
#                                   next step starts while Vapor still works
set -u
R='/Volumes/My Shared Files/results'
T="$R/tools"
APP="${APP:-$HOME/Vapor/webapp}"
CACHE="$HOME/fixtures/npm-cache"
NPM=(npm --offline --cache "$CACHE" --no-audit --no-fund --ignore-scripts --loglevel=error)
export npm_config_userconfig=/dev/null
export npm_config_update_notifier=false

now() { python3 -c 'import time; print(f"{time.time():.2f}")'; }
since() { python3 -c "import time,sys; print(f'{time.time()-float(sys.argv[1]):.1f}')" "$1"; }

count() {
  local n=0 d=0
  [ -d "$APP/node_modules" ] && n=$(find "$APP/node_modules" -type f | wc -l | tr -d ' ') && d=$(find "$APP/node_modules" -type d | wc -l | tr -d ' ')
  echo "files=$n dirs=$d"
}

quiet() {
  local t0=$(now)
  python3 "$T/vt.py" wait --timeout "${1:-1800}" --quiet 10 | tail -1 | cut -c1-260
  echo "   vapor quiet after $(since $t0)s"
}

step() {
  local label="$1"; shift
  local t0=$(now)
  "$@" > /tmp/wl_npm.out 2>&1
  local code=$?
  echo "== $label: exit $code in $(since $t0)s; $(count)"
  [ $code -ne 0 ] && tail -5 /tmp/wl_npm.out
  return 0
}

case "${1:-}" in
  setup)
    mkdir -p "$HOME/fixtures" "$APP/src"
    [ -d "$CACHE" ] || tar -xf "$R/fixtures/npm-cache.tar" -C "$HOME/fixtures"
    cp "$R/fixtures/webapp/package.json" "$R/fixtures/webapp/package-lock.json" "$APP/"
    printf 'export const app = "webapp";\n' > "$APP/src/index.ts"
    echo "setup done: $(du -sh "$CACHE" | cut -f1) cache; node $(node --version), npm $(npm --version)"
    ;;
  ci) cd "$APP" && step "npm ci" "${NPM[@]}" ci; quiet ;;
  rm) cd "$APP" && step "rm -rf node_modules" rm -rf node_modules; quiet ;;
  uninstall) shift; cd "$APP" && step "npm uninstall $*" "${NPM[@]}" uninstall "$@"; quiet ;;
  install) shift; cd "$APP" && step "npm install $*" "${NPM[@]}" install "$@"; quiet ;;
  cycle)
    n="${2:-3}"; mode="${3:-wait}"
    cd "$APP"
    for i in $(seq 1 "$n"); do
      step "cycle $i: npm ci" "${NPM[@]}" ci
      [ "$mode" = wait ] && quiet || sleep "${GAP:-3}"
      step "cycle $i: rm -rf node_modules" rm -rf node_modules
      [ "$mode" = wait ] && quiet || sleep "${GAP:-3}"
    done
    [ "$mode" = wait ] || quiet
    ;;
  *) echo "usage: wl_npm.sh setup|ci|rm|uninstall|install|cycle"; exit 2 ;;
esac
