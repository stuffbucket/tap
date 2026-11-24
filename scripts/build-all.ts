#!/usr/bin/env bun

async function run(script: string): Promise<void> {
  const child = Bun.spawn([process.execPath, "run", script], {
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  })
  if ((await child.exited) !== 0) throw new Error(`${script} failed`)
}

await run("build:icon")
await run("build:native")
await run("build:vhs")
if (process.platform === "darwin") {
  await run("build:ghostty-app")
  await run("build:ghostty-full")
}
