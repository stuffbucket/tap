import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises"
import { randomUUID } from "node:crypto"
import { dirname, resolve } from "node:path"
import { validateProject, type TapProject } from "./model.ts"

export async function readProject(path: string): Promise<TapProject> {
  const parsed: unknown = JSON.parse(await readFile(resolve(path), "utf8"))
  validateProject(parsed)
  return parsed
}

export async function writeProject(path: string, project: TapProject): Promise<void> {
  validateProject(project)
  const target = resolve(path)
  const temporary = `${target}.${process.pid}.${randomUUID()}.tmp`
  await mkdir(dirname(target), { recursive: true })
  try {
    await writeFile(temporary, `${JSON.stringify(project, null, 2)}\n`, "utf8")
    await rename(temporary, target)
  } finally {
    await rm(temporary, { force: true })
  }
}
