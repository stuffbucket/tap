import { expect, test } from "bun:test"
import { decodeEventBytes, encodeBytes } from "../src/bytes.ts"

test("PTY bytes round-trip without UTF-8 loss", () => {
  const source = Uint8Array.from([0, 27, 255, 65])
  const encoded = encodeBytes(source)
  expect(Array.from(decodeEventBytes({ atUs: 0, type: "output", ...encoded }))).toEqual(Array.from(source))
})
