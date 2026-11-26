import { existsSync } from "node:fs"
import { join, resolve } from "node:path"

/** The checkout `tap` runs from. */
const ROOT = resolve(import.meta.dir, "..")

/** How to build the Rust indexer; printed when it is missing. */
export const BUILD_TAP_INDEX = "mise x rust@1.99.0 zig@0.17.1 -- cargo build --release -p tap-index"

/**
 * The `tap-index` binary built from crates/tap-index, preferring a release build. It is not part of
 * Tap.app yet: the macOS release builder has no Rust toolchain, so `tap transcript` works only from
 * a source checkout where it has been built.
 */
export function tapIndexBinary(root = ROOT, env: Record<string, string | undefined> = process.env): string {
  const name = process.platform === "win32" ? "tap-index.exe" : "tap-index"
  const targets = [env.CARGO_TARGET_DIR, join(root, "target")].filter((dir): dir is string => Boolean(dir))
  for (const target of targets) {
    for (const profile of ["release", "debug"]) {
      const candidate = resolve(root, target, profile, name)
      if (existsSync(candidate)) return candidate
    }
  }
  throw new Error(
    `tap transcript needs the tap-index binary, which has not been built (looked in ${targets
      .map((target) => join(target, "{release,debug}"))
      .join(" and ")}). Build it from the checkout with:\n  ${BUILD_TAP_INDEX}`,
  )
}

/** Runs `tap-index transcript`, passing its output through. Returns its exit code. */
export async function printTranscript(recording: string, options: { json?: boolean } = {}): Promise<number> {
  const binary = tapIndexBinary()
  const child = Bun.spawn([binary, "transcript", resolve(recording), ...(options.json ? ["--json"] : [])], {
    stdin: "ignore",
    stdout: "inherit",
    stderr: "inherit",
  })
  return await child.exited
}
