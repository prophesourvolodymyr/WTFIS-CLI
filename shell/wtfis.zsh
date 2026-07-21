wtfis() {
  local output exit_code selected
  output="$(mktemp "${TMPDIR:-/tmp}/wtfis.XXXXXX")" || return
  WTFIS_OUTPUT="$output" command wtfis "$@" </dev/tty >/dev/tty 2>/dev/tty
  exit_code=$?
  if [ "$exit_code" -eq 0 ] && [ -s "$output" ]; then
    selected="$(<"$output")"
    [ -n "$selected" ] && builtin cd -- "$selected"
  fi
  rm -f "$output"
  return "$exit_code"
}

cdd() { wtfis "$@"; }
