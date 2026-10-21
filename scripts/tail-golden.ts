// The plugin's readTail cut over seeded cases, so `read_tail` is held to JavaScript's own
// answer: `bun scripts/tail-golden.ts > fixtures/parity/tail-golden.json`.
// The expression is the plugin's line verbatim (server/loops.ts, readTail); parity-check.sh fails
// if the pinned plugin no longer has it.
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
// One char of each UTF-8 width, a surrogate pair (two units), and line breaks.
const alphabet = ["a", "b", " ", "\n", "\n", "é", "€", "🚀"];
const cases: { text: string; limit: number }[] = [
  { text: "", limit: 4 },
  { text: "abc\n", limit: 4 },
  { text: "abcd\nef\n", limit: 4 },
  { text: "abc\ndef\n", limit: 4 },
  { text: "aaaaaaa\nbbbbbbbbbbbb", limit: 4 },
  { text: "x\n🚀🚀\nyy\n", limit: 5 },
  { text: "€€€€€€€€€€€€\n€\n", limit: 3 },
];
for (let i = 0; i < 400; i++) {
  const length = next() % 120;
  let text = "";
  for (let j = 0; j < length; j++) text += alphabet[next() % alphabet.length];
  cases.push({ text, limit: 1 + (next() % 40) });
}
const bytes = (s: string) => Buffer.byteLength(s, "utf8");
const out = cases.map(({ text, limit }) => {
  const tail = cut(text, limit);
  return { text, limit, tail, from: tail === text ? 0 : bytes(text) - bytes(tail) };
});
console.log(JSON.stringify(out, null, 1));
