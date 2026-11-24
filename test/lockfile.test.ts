import { expect, test } from "bun:test"
import { resolve } from "node:path"

// Packages from the default registry are recorded with an empty URL, so each machine resolves them
// against its own configured registry (a local proxy, or npmjs on the macOS builder). A full URL
// would pin installs to whichever registry generated the lockfile, which another machine may not
// be able to reach.
test("bun.lock does not pin a registry", async () => {
  const lock = await Bun.file(resolve(import.meta.dir, "../bun.lock")).text()
  expect(lock.match(/"https?:\/\/[^"]+\.tgz"/g) ?? []).toEqual([])
})
