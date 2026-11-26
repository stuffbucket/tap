import { afterAll, beforeAll, expect, test } from "bun:test"
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { detectShell, withShellIntegration } from "../src/shell-integration.ts"
import { captureProject } from "../src/capture.ts"

const ESC = "\x1b"
const BEL = "\x07"
const mark = (body: string) => `${ESC}]133;${body}${BEL}`

let home = ""

beforeAll(async () => {
  home = await mkdtemp(join(tmpdir(), "tap-shell-integration-"))
})

afterAll(async () => {
  await rm(home, { recursive: true, force: true })
})

test("detects zsh and fish from the program name, including login shells", () => {
  expect(detectShell(["/bin/zsh", "-l"])).toBe("zsh")
  expect(detectShell(["-zsh"])).toBe("zsh")
  expect(detectShell(["/opt/homebrew/bin/fish"])).toBe("fish")
  expect(detectShell(["/bin/bash"])).toBeNull()
  expect(detectShell(["vim", "zsh"])).toBeNull()
  expect(detectShell([])).toBeNull()
})

test("points zsh at the shim and remembers the user's ZDOTDIR", () => {
  const { env, shell } = withShellIntegration(["zsh"], { ZDOTDIR: "/u/zdot", HOME: "/u" }, { commandLines: true })
  expect(shell).toBe("zsh")
  expect(env.ZDOTDIR).toEndWith(join("shell-integration", "zsh"))
  expect(env.TAP_ZSH_ZDOTDIR).toBe("/u/zdot")
  expect(env.TAP_SHELL_INTEGRATION_CMDLINE).toBe("1")

  const unset = withShellIntegration(["zsh"], { HOME: "/u", TAP_ZSH_ZDOTDIR: "/stale" }, { commandLines: false })
  expect("TAP_ZSH_ZDOTDIR" in unset.env).toBe(false)
  expect(unset.env.TAP_SHELL_INTEGRATION_CMDLINE).toBe("0")
})

test("leaves other programs and fish untouched", () => {
  const env = { HOME: "/u" }
  expect(withShellIntegration(["bash"], env, { commandLines: true })).toEqual({ env, shell: null })
  expect(withShellIntegration(["fish"], env, { commandLines: true })).toEqual({ env, shell: "fish" })
})

/** Runs an interactive zsh in a PTY with the shim, types each line after a prompt, returns all output. */
async function runZsh(
  lines: string[],
  options: { home: string; zdotdir?: string; commandLines?: boolean },
): Promise<string> {
  const base: Record<string, string | undefined> = {
    PATH: process.env.PATH,
    HOME: options.home,
    TERM: "xterm-ghostty",
    ...(options.zdotdir ? { ZDOTDIR: options.zdotdir } : {}),
  }
  const { env } = withShellIntegration(["zsh"], base, { commandLines: options.commandLines ?? true })
  let output = ""
  const child = Bun.spawn(["zsh", "-i"], {
    env: Object.fromEntries(Object.entries(env).filter((entry): entry is [string, string] => entry[1] !== undefined)),
    terminal: { cols: 100, rows: 30, data: (_terminal, data) => { output += new TextDecoder().decode(data) } },
  })
  // Every test's .zshrc sets PS1 to "PROMPT> ", so prompts are counted by their text, which works
  // whether or not marks are written.
  const prompts = () => output.split("PROMPT> ").length - 1
  const waitFor = async (predicate: () => boolean, what: string) => {
    for (let waited = 0; !predicate(); waited += 10) {
      if (waited > 5_000) throw new Error(`timed out waiting for ${what}; output so far: ${JSON.stringify(output)}`)
      await Bun.sleep(10)
    }
  }
  // A line is typed only once the previous prompt has been drawn, as a person would.
  let seen = 0
  for (const line of lines) {
    await waitFor(() => prompts() > seen, "a prompt")
    seen = prompts()
    child.terminal!.write(`${line}\r`)
    await waitFor(() => prompts() > seen, `the prompt after ${JSON.stringify(line)}`)
  }
  child.terminal!.write("exit\r")
  await child.exited
  return output
}

async function userHome(name: string, files: Record<string, string>): Promise<string> {
  const dir = join(home, name)
  await mkdir(dir, { recursive: true })
  for (const [file, body] of Object.entries(files)) await writeFile(join(dir, file), body)
  return dir
}

const USER_FILES = {
  ".zshenv": "export TAP_TEST_ZSHENV=ran\n",
  ".zshrc": "PS1='PROMPT> '\nexport TAP_TEST_ZSHRC=ran\nprecmd_functions+=(user_precmd)\nuser_precmd() { print -n 'USER-PRECMD ' }\n",
}

test("marks prompts, command lines and exit statuses without touching the user's startup", async () => {
  const dir = await userHome("plain", USER_FILES)
  const output = await runZsh([
    `print -r -- "env=$TAP_TEST_ZSHENV rc=$TAP_TEST_ZSHRC zd=\${ZDOTDIR-unset} left=\${TAP_ZSH_ZDOTDIR-none}"`,
    "echo hi | cat",
    "false",
  ], { home: dir })

  // The user's own .zshenv and .zshrc ran, and Tap's ZDOTDIR and variables are gone.
  expect(output).toContain("env=ran rc=ran zd=unset left=none")
  // The prompt theme is the user's. After the first prompt, the P mark comes after the user's own
  // precmd output and right before zsh draws the prompt.
  expect(output).toContain(`USER-PRECMD ${mark("P")}\rPROMPT> `)
  expect(output).toContain(mark("C;cmdline_url=echo%20hi%20%7C%20cat"))
  const echo = output.indexOf(mark("C;cmdline_url=echo%20hi%20%7C%20cat"))
  const hi = output.indexOf("hi\r\n", echo)
  const done = output.indexOf(mark("D;0"), echo)
  expect(echo).toBeGreaterThan(-1)
  expect(hi).toBeGreaterThan(echo)
  expect(done).toBeGreaterThan(hi)
  // zsh's partial-line marker (PROMPT_SP) is printed before precmd, so it sits inside the command's
  // output region, as it does with every OSC 133 integration.
  const falseRan = output.indexOf(mark("C;cmdline_url=false"))
  expect(falseRan).toBeGreaterThan(-1)
  expect(output.indexOf(mark("D;1"), falseRan)).toBeGreaterThan(falseRan)
  expect(output.indexOf(mark("D;0"), falseRan)).toBe(-1)
  // No mark asks the terminal for a fresh line or moves the cursor.
  expect(output).not.toContain(mark("A"))
})

test("restores a custom ZDOTDIR and reads the user's files from it", async () => {
  const dir = await userHome("zdot-home", {})
  const zdotdir = await userHome("zdot-dir", USER_FILES)
  const output = await runZsh([`print -r -- "zd=$ZDOTDIR rc=$TAP_TEST_ZSHRC"`], { home: dir, zdotdir })
  expect(output).toContain(`zd=${zdotdir} rc=ran`)
  expect(output).toContain(mark("P"))
})

test("omits command lines when input is not recorded", async () => {
  const dir = await userHome("redacted", USER_FILES)
  const output = await runZsh(["echo secret-token"], { home: dir, commandLines: false })
  expect(output).toContain(`${mark("C")}`)
  expect(output).not.toContain("cmdline_url")
  expect(output).not.toContain("secret-token" + BEL)
})

test("steps aside when another integration already writes OSC 133", async () => {
  const dir = await userHome("other-integration", {
    ".zshrc": "PS1='PROMPT> '\n_ghostty_precmd() { : }\nprecmd_functions+=(_ghostty_precmd)\n",
  })
  const output = await runZsh(["echo hi"], { home: dir })
  expect(output).toContain("PROMPT> ")
  expect(output).not.toContain(`${ESC}]133;`)
})

test("capture records which shells were integrated, and can be turned off", async () => {
  const pane = { id: "main", command: ["zsh", "-c", "true"], env: { HOME: home } }
  const on = await captureProject({ panes: [pane] }, { interactive: false, mirrorOutput: false })
  expect(on.metadata.shellIntegration).toEqual(["zsh"])
  const off = await captureProject({ panes: [pane] }, { interactive: false, mirrorOutput: false, shellIntegration: false })
  expect(off.metadata.shellIntegration).toBeUndefined()
})
