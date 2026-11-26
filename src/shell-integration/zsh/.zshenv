# Tap shell integration for zsh.
#
# Tap starts zsh with ZDOTDIR pointing at this directory, so zsh reads this
# file first. It hands ZDOTDIR back to the user straight away, which makes zsh
# read the user's own .zprofile, .zshrc and .zlogin afterwards as if Tap had
# never been involved. Interactive shells then load tap-integration, which
# marks prompts and commands with OSC 133 so a recording can be split into
# commands.

if [[ -n "${TAP_ZSH_ZDOTDIR+set}" ]]; then
  builtin export ZDOTDIR="$TAP_ZSH_ZDOTDIR"
else
  builtin unset ZDOTDIR
fi
builtin unset TAP_ZSH_ZDOTDIR

# The user's .zshenv runs before the hooks are defined, because it may set
# fpath or options the rest of their startup depends on.
if [[ -r "${ZDOTDIR:-$HOME}/.zshenv" && ! -d "${ZDOTDIR:-$HOME}/.zshenv" ]]; then
  builtin source -- "${ZDOTDIR:-$HOME}/.zshenv"
fi

if [[ -o interactive && -r "${${(%):-%x}:A:h}/tap-integration" ]]; then
  builtin source -- "${${(%):-%x}:A:h}/tap-integration"
fi
