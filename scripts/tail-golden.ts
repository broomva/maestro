// The plugin's readTail over seeded byte cases, so `read_tail` is held to JavaScript's own
// answer: `bun scripts/tail-golden.ts > fixtures/parity/tail-golden.json`.
// The cut is the plugin's line verbatim (server/loops.ts, readTail), and the bytes decode as its
// `readFile(path, "utf8")` decodes them; parity-check.sh fails if the pinned plugin changes the line.
const cut = (text: string, limit: number | null) =>
  limit !== null && text.length > limit ? text.slice(text.indexOf("\n", text.length - limit) + 1) : text;

let seed = 3215;
// xorshift32 in 32-bit integers: a double-precision LCG would lose its low bits.
const next = () => {
  seed ^= seed << 13;
  seed ^= seed >>> 17;
  seed ^= seed << 5;
  return seed >>> 0;
};
const utf8 = (s: string) => Buffer.from(s, "utf8");
// One char of each UTF-8 width, a surrogate pair (two units), line breaks, and invalid bytes:
// a lone 0xFF, a lone continuation byte, a 3-byte lead cut short.
const alphabet: Buffer[] = ["a", "b", " ", "\n", "\n", "é", "€", "🚀"].map(utf8);
const invalid: Buffer[] = [Buffer.from([0xff]), Buffer.from([0x80]), Buffer.from([0xe2, 0x82])];
const cases: { bytes: Buffer; limit: number }[] = [
  ["", 4],
  ["abc\n", 4],
  ["abcd\nef\n", 4],
  ["abc\ndef\n", 4],
  ["aaaaaaa\nbbbbbbbbbbbb", 4],
  ["x\n🚀🚀\nyy\n", 5],
  ["€€€€€€€€€€€€\n€\n", 3],
  // A 4-byte char whose lead byte sits just before a 3 * limit byte window, then 3-byte chars only:
  // the window holds limit - 1 units unless it is 3 bytes wider.
  ["xxxxx🚀€€€", 4],
  ["xxxxx🚀€€€\n€", 4],
  ["x\nxxxx🚀€€€€", 5],
].map(([s, limit]) => ({ bytes: utf8(s as string), limit: limit as number }));
cases.push({ bytes: Buffer.from("aaaa\n\xff\nb\n", "latin1"), limit: 4 });
cases.push({ bytes: Buffer.from("aa\n\xffb\n", "latin1"), limit: 4 });
for (let i = 0; i < 600; i++) {
  const length = next() % 120;
  const parts: Buffer[] = [];
  const pool = i % 3 === 0 ? [...alphabet, ...invalid] : alphabet;
  for (let j = 0; j < length; j++) parts.push(pool[next() % pool.length] as Buffer);
  cases.push({ bytes: Buffer.concat(parts), limit: 1 + (next() % 40) });
}
const newlines = (b: Buffer) => b.reduce((n, x) => n + (x === 0x0a ? 1 : 0), 0);
const out = cases.map(({ bytes, limit }) => {
  const text = bytes.toString("utf8");
  const tail = cut(text, limit);
  // The tail starts after a line break, so the cut in bytes is after the same count of 0x0A bytes.
  let from = 0;
  if (tail !== text) {
    let skip = newlines(bytes) - newlines(utf8(tail));
    while (skip > 0) if (bytes[from++] === 0x0a) skip--;
  }
  return { hex: bytes.toString("hex"), limit, tail, from };
});
console.log(JSON.stringify(out, null, 1));
