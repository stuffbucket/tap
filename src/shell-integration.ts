import { basename, join } from "node:path"

/** Shells whose prompts and commands Tap can mark with OSC 133. */
export type IntegratedShell = "zsh" | "fish"

export interface ShellIntegrationOptions {
  /**
   * Include each command line in its OSC 133 C mark. Only safe when input is recorded:
   * otherwise a redacted or omitted command would reappear in the output stream.
   */
  commandLines: boolean
}

const INTEGRATION_DIR = join(import.meta.dir, "shell-integration")

/** The shell a pane runs, from its program name (`/bin/zsh`, `-zsh`, `zsh`). */
export function detectShell(command: readonly string[]): IntegratedShell | null {
  const program = command[0]
  if (!program) return null
  const name = basename(program).replace(/^-/, "")
  if (name === "zsh") return "zsh"
  if (name === "fish") return "fish"
  return null
}

/**
 * Returns the environment a pane's command should start with so its shell marks prompts and
 * commands with OSC 133, or the environment unchanged when the command is not a supported shell.
 *
 * zsh is pointed at Tap's ZDOTDIR shim, which restores the user's own ZDOTDIR before any of their
 * startup files run. fish 4 and newer writes OSC 133 itself, so it needs nothing.
 */
export function withShellIntegration(
  command: readonly string[],
  env: Record<string, string | undefined>,
  options: ShellIntegrationOptions,
): { env: Record<string, string | undefined>; shell: IntegratedShell | null } {
  const shell = detectShell(command)
  if (shell !== "zsh") return { env, shell }
  const next: Record<string, string | undefined> = { ...env }
  if (env.ZDOTDIR !== undefined) next.TAP_ZSH_ZDOTDIR = env.ZDOTDIR
  else delete next.TAP_ZSH_ZDOTDIR
  next.ZDOTDIR = join(INTEGRATION_DIR, "zsh")
  next.TAP_SHELL_INTEGRATION_CMDLINE = options.commandLines ? "1" : "0"
  return { env: next, shell }
}
