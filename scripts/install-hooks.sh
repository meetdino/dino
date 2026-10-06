#!/bin/sh
# Keep the dino you use (~/Applications/Dino.app) built from main on this Mac: installs the git
# hooks that run scripts/dev-rebuild.sh when main moves in the main checkout, which
# scripts/land.sh also fast-forwards from any worktree. Run once, from any checkout.
#   scripts/install-hooks.sh            install
#   scripts/install-hooks.sh --remove   remove
set -eu
hooks=$(git rev-parse --path-format=absolute --git-path hooks)
mkdir -p "$hooks"
for h in post-commit post-merge; do
    if [ -e "$hooks/$h" ] && ! grep -qs 'dev-rebuild\|dino-rebuild' "$hooks/$h"; then
        echo "$hooks/$h is a hook of your own: add a line to it that runs scripts/dev-rebuild.sh" >&2
        continue
    fi
    if [ "${1:-}" = --remove ]; then rm -f "$hooks/$h"; continue; fi
    cat >"$hooks/$h" <<'EOF'
#!/bin/sh
# From scripts/install-hooks.sh: rebuild the dino you use when main moves in the main checkout.
s="$(git rev-parse --show-toplevel)/scripts/dev-rebuild.sh"
if [ -x "$s" ]; then exec "$s"; fi
EOF
    chmod +x "$hooks/$h"
done
rm -f "$hooks/dino-rebuild"   # what the hooks ran before scripts/dev-rebuild.sh
