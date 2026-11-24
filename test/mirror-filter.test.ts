import { expect, test } from "bun:test"
import { QueryFilter, isTerminalQuery } from "../src/mirror-filter.ts"

const filter = (...chunks: string[]) => {
  const instance = new QueryFilter()
  return chunks.map((chunk) => Buffer.from(instance.push(Buffer.from(chunk, "latin1"))).toString("latin1")).join("")
}

test("queries a shell sends at startup are not shown to the user's terminal", () => {
  // zsh prompt themes and plugins ask for DA1/DA2 and the cursor position; Terminal.app answers
  // "\e[?1;2c\e[>1;95;0c", which used to arrive as typed input.
  expect(filter("hello\x1b[c\x1b[>c\x1b[6n world")).toBe("hello world")
})

test("every kind of query is recognised", () => {
  for (const query of [
    "\x1b[c", "\x1b[0c", "\x1b[>c", "\x1b[>0c", "\x1b[=c", "\x1bZ",
    "\x1b[5n", "\x1b[6n", "\x1b[?6n", "\x1b[?996n",
    "\x1b[?2026$p", "\x1b[4$p", "\x1b[>q", "\x1b[>0q", "\x1b[?u", "\x1b[?1;1S",
    "\x1b[14t", "\x1b[18t", "\x1b[16t",
    "\x1b]10;?\x07", "\x1b]11;?\x1b\\", "\x1b]4;1;?\x07", "\x1b]52;c;?\x07",
    "\x1bP$qm\x1b\\", "\x1bP+q544e\x1b\\", "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\",
  ]) expect([query, isTerminalQuery(query)]).toEqual([query, true])
})

test("look-alike sequences that change state pass through", () => {
  for (const sequence of [
    "\x1b[2 q", "\x1b[>4;1m", "\x1b[>1u", "\x1b[<u", "\x1b[=1;1u", "\x1b[>1n",
    "\x1b[8;24;80t", "\x1b[22;0t", "\x1b]0;title\x07", "\x1b]8;;https://example.com\x1b\\",
    "\x1b]52;c;aGk=\x07", "\x1b]11;#101014\x07", "\x1b_Ga=T,f=100;iVBOR\x1b\\", "\x1b[?1049h", "\x1b(B", "\x1b7",
  ]) expect(filter(sequence)).toBe(sequence)
})

test("sequences split across chunks are handled", () => {
  expect(filter("a\x1b", "[", "6", "nb\x1b]1", "1;?", "\x07c\x1b[3", "1mred")).toBe("abc\x1b[31mred")
})

test("text and UTF-8 pass through byte-for-byte", () => {
  const text = Buffer.from("héllo ✓ 🙂 ┌─┐\r\n", "utf8").toString("latin1")
  expect(filter(text)).toBe(text)
})
