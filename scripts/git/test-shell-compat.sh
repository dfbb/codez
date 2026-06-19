#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failed=0

for script in "$SCRIPT_DIR"/*.zsh; do
  [ -f "$script" ] || continue
  if head -n 1 "$script" | grep -q 'zsh'; then
    echo "Error: $(basename "$script") uses a zsh shebang."
    failed=1
  fi

  if grep -Eq '\$\{[^}]+:[Ahht]+[^}]*\}' "$script"; then
    echo "Error: $(basename "$script") uses zsh-only parameter expansion."
    failed=1
  fi

  echo "bash -n $(basename "$script")"
  if ! bash -n "$script"; then
    failed=1
  fi

  echo "zsh -n $(basename "$script")"
  if ! zsh -n "$script"; then
    failed=1
  fi
done

exit "$failed"
