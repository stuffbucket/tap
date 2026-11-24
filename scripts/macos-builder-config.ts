import { readFile } from "node:fs/promises"
import { resolve } from "node:path"

/** The keys Tap uses from `.macos-builder/config` (flat `key = value`, `#` comments). */
export interface BuilderConfig {
  app_path: string
  volname: string
  bundle_id: string
  entitlements: string
  artifact: string
  dmg_name?: string
  sign_walk?: string
  sign_nested?: string
}

export const builderConfigPath = resolve(import.meta.dir, "../.macos-builder/config")

export async function readBuilderConfig(): Promise<BuilderConfig> {
  const values: Record<string, string> = {}
  for (const raw of (await readFile(builderConfigPath, "utf8")).split("\n")) {
    const line = raw.trim()
    if (!line || line.startsWith("#")) continue
    const match = line.match(/^([a-z_]+)\s*=\s*(.*)$/)
    if (!match) throw new Error(`.macos-builder/config: cannot parse line: ${raw}`)
    values[match[1]!] = match[2]!.trim()
  }
  for (const key of ["app_path", "volname", "bundle_id", "entitlements"]) {
    if (!values[key]) throw new Error(`.macos-builder/config: missing required key ${key}`)
  }
  return { artifact: "dmg", ...values } as unknown as BuilderConfig
}

/** The builder's dmg name: `dmg_name` with `{version}` and `{tag}` substituted, or `<volname>.dmg`. */
export function dmgName(config: BuilderConfig, version: string): string {
  return (config.dmg_name ?? `${config.volname}.dmg`).replaceAll("{version}", version).replaceAll("{tag}", `v${version}`)
}
