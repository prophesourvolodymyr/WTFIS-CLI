wtfis() {
  local selected
  selected="$(command wtfis "$@" </dev/tty 2>/dev/tty)" || return
  [ -n "$selected" ] && builtin cd -- "$selected"
}

cdd() { wtfis "$@"; }
