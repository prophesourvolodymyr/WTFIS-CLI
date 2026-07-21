wtfis() {
  local output status selected
  output="$(mktemp "${TMPDIR:-/tmp}/wtfis.XXXXXX")" || return
  WTFIS_OUTPUT="$output" command wtfis "$@" >/dev/tty 2>/dev/tty
  status=$?
  if [ "$status" -eq 0 ] && [ -s "$output" ]; then
    selected="$(<"$output")"
    [ -n "$selected" ] && builtin cd -- "$selected"
  fi
  rm -f "$output"
  return "$status"
}

cdd() { wtfis "$@"; }
