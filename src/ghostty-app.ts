import { existsSync } from "node:fs"
import { resolve } from "node:path"

/**
 * Ghostty.app is pinned to the exact commit of the libghostty-vt release Tap builds against
 * (tag vt-1.0.1), so the Metal renderer and Tap's VT state come from the same Ghostty source.
 * No Ghostty.app release contains that commit, so Tap uses Ghostty's official per-commit build
 * (Developer ID signed by Ghostty's author and notarized), fetched by `bun run build:ghostty-app`.
 */
export const PINNED_VT_TAG = "vt-1.0.1"
export const PINNED_GHOSTTY_COMMIT = "108cacb17ed8990840cd14abfce43d8ed8503ca1"
export const GHOSTTY_TEAM_ID = "24VZTF6M5V"
export const pinnedGhosttyDmgUrl = `https://tip.files.ghostty.org/${PINNED_GHOSTTY_COMMIT}/Ghostty.dmg`
export const pinnedGhosttyDirectory = resolve(import.meta.dir, `../.cache/ghostty-app-${PINNED_GHOSTTY_COMMIT}`)
export const pinnedGhosttyApp = resolve(pinnedGhosttyDirectory, "Ghostty.app")
const installedGhosttyApp = "/Applications/Ghostty.app"

/**
 * The Ghostty.app Tap renders with: the bundle's copy (TAP_GHOSTTY_STOCK, set by the DMG launcher),
 * then the pinned build in the source checkout, then an installed Ghostty.app as a fallback.
 */
export function resolveGhosttyApp(): { executable: string; frameworks: string; source: "bundle" | "pinned" | "installed" } {
  if (process.env.TAP_GHOSTTY_STOCK) {
    const executable = process.env.TAP_GHOSTTY_STOCK
    return { executable, frameworks: process.env.TAP_GHOSTTY_FRAMEWORKS ?? resolve(executable, "../../Frameworks"), source: "bundle" }
  }
  const app = existsSync(pinnedGhosttyApp) ? pinnedGhosttyApp : installedGhosttyApp
  return {
    executable: resolve(app, "Contents/MacOS/ghostty"),
    frameworks: resolve(app, "Contents/Frameworks"),
    source: app === pinnedGhosttyApp ? "pinned" : "installed",
  }
}

/** The Ghostty commit a Ghostty.app was built from (its Info.plist `GhosttyCommit`), if readable. */
export async function ghosttyAppCommit(executable: string): Promise<string | null> {
  const plist = resolve(executable, "../../Info.plist")
  if (!existsSync(plist)) return null
  const child = Bun.spawn(["/usr/bin/plutil", "-extract", "GhosttyCommit", "raw", "-o", "-", plist], { stdout: "pipe", stderr: "ignore" })
  const value = (await new Response(child.stdout).text()).trim()
  return (await child.exited) === 0 && value ? value : null
}

export const matchesPinnedCommit = (commit: string | null) => !!commit && PINNED_GHOSTTY_COMMIT.startsWith(commit)
