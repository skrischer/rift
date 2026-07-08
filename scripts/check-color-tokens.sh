#!/usr/bin/env bash
# Hex->token sweep regression guard (docs/spec-settings-theme.md): fails if a
# raw color constructor reappears in a product rendering path instead of a
# `gpui-component` theme token read through `cx.theme()`.
#
# Scope: crates/app/src and crates/terminal/src are the only crates that
# depend on gpui (rendering paths); every other crate is backend and has no
# color constructors at all.
#
# Allowlist: a `color-token-allow: <reason>` comment within a few lines of the
# match documents a genuine exception (the xterm 6x6x6 cube / grayscale ramp,
# which is an exact terminal standard, not a theme palette) and is excluded.
#
# Usage: scripts/check-color-tokens.sh
set -euo pipefail

paths=(crates/app/src crates/terminal/src)

# rgb(/rgba(/hsla( function calls, Rgba{/Hsla{ struct literals, white()/black().
# `\bHsla\s*\{`/`\bRgba\s*\{` also match a `-> Hsla {` function-return
# signature (not a color literal); those lines are skipped below since a real
# struct literal never contains `->`.
pattern='\brgb\s*\(|\brgba\s*\(|\bhsla\s*\(|\bRgba\s*\{|\bHsla\s*\{|\bwhite\(\)|\bblack\(\)'

fail=0
while IFS=: read -r file line _; do
  content=$(sed -n "${line}p" "$file")
  case "$content" in
  *'->'*) continue ;;
  esac

  window_start=$((line > 3 ? line - 3 : 1))
  window_end=$((line + 2))
  if sed -n "${window_start},${window_end}p" "$file" | grep -q 'color-token-allow:'; then
    continue
  fi

  echo "$file:$line: $content"
  fail=1
done < <(grep -rnE "$pattern" "${paths[@]}" --include='*.rs')

if [ "$fail" -ne 0 ]; then
  echo >&2
  echo "error: raw color constructor(s) found outside theme tokens (see above)." >&2
  echo "Use a gpui-component theme token via cx.theme() instead, or add a" >&2
  echo "'color-token-allow: <reason>' comment near the line if genuinely" >&2
  echo "non-themeable (docs/spec-settings-theme.md)." >&2
  exit 1
fi

echo "check-color-tokens: no raw color constructors found in product rendering paths"
