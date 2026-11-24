// TextEncoder and TextDecoder (crates/temzdf-script/src/text_codec.ts and
// text_codec.rs): the WHATWG Encoding Standard's UTF-8, the cases of the
// Web Platform Tests that matter here.
import { test } from "temzdf/test";

const bytes = (...b: number[]): Uint8Array => new Uint8Array(b);
const list = (b: Uint8Array): number[] => Array.from(b);

test("encoding and decoding go both ways", (assert) => {
  const text = "aéあ\u{1f600}";
  const encoded = new TextEncoder().encode(text);
  assert.deepEqual(list(encoded), [0x61, 0xc3, 0xa9, 0xe3, 0x81, 0x82, 0xf0, 0x9f, 0x98, 0x80]);
  assert.equal(new TextDecoder().decode(encoded), text);
  assert.equal(new TextEncoder().encoding, "utf-8");
  assert.deepEqual(list(new TextEncoder().encode()), [], "nothing: no bytes");
  assert.equal(new TextDecoder().decode(), "", "nothing: no text");
});

test("lone surrogates are encoded as U+FFFD", (assert) => {
  assert.deepEqual(list(new TextEncoder().encode("a\ud800b")), [0x61, 0xef, 0xbf, 0xbd, 0x62]);
  assert.deepEqual(list(new TextEncoder().encode("\udc00")), [0xef, 0xbf, 0xbd]);
});

test("invalid bytes become U+FFFD, or a TypeError when fatal", (assert) => {
  const d = new TextDecoder();
  assert.equal(d.decode(bytes(0x61, 0x80, 0x62)), "a�b", "a lone continuation byte");
  assert.equal(d.decode(bytes(0xe3, 0x81)), "�", "a truncated sequence at the end");
  assert.equal(d.decode(bytes(0xc0, 0xaf)), "��", "an overlong form");
  assert.equal(d.decode(bytes(0xed, 0xa0, 0x80)), "���", "an encoded surrogate");
  const fatal = new TextDecoder("utf-8", { fatal: true });
  assert.true(fatal.fatal);
  assert.throws(() => fatal.decode(bytes(0x61, 0x80)), TypeError);
  assert.equal(fatal.decode(bytes(0x61)), "a", "usable after a failure");
});

test("a byte order mark at the start is dropped unless ignoreBOM", (assert) => {
  const bom = bytes(0xef, 0xbb, 0xbf, 0x61);
  assert.equal(new TextDecoder().decode(bom), "a");
  assert.equal(new TextDecoder("utf-8", { ignoreBOM: true }).decode(bom), "﻿a");
  const d = new TextDecoder();
  assert.equal(d.decode(bytes(0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf)), "﻿", "only the first");
  // In a stream: only at its start, even split.
  assert.equal(d.decode(bytes(0xef, 0xbb), { stream: true }), "");
  assert.equal(d.decode(bytes(0xbf, 0x61), { stream: true }), "a");
  assert.equal(d.decode(bytes(0xef, 0xbb, 0xbf)), "﻿", "not at the stream's start");
  assert.equal(d.decode(bom), "a", "a new stream after one ends");
});

test("a stream keeps a character split across calls", (assert) => {
  const encoded = new TextEncoder().encode("aあ\u{1f600}b");
  const d = new TextDecoder();
  let text = "";
  for (const b of encoded) text += d.decode(bytes(b), { stream: true });
  text += d.decode();
  assert.equal(text, "aあ\u{1f600}b", "one byte at a time");
  assert.equal(d.decode(bytes(0xe3, 0x81), { stream: true }), "");
  assert.equal(d.decode(), "�", "incomplete at the stream's end");
});

test("views, buffers and data views are decoded where they point", (assert) => {
  const buffer = new TextEncoder().encode("xabcx").buffer;
  assert.equal(new TextDecoder().decode(buffer), "xabcx");
  assert.equal(new TextDecoder().decode(new Uint8Array(buffer, 1, 3)), "abc");
  assert.equal(new TextDecoder().decode(new DataView(buffer, 1, 3)), "abc");
  assert.equal(new TextDecoder().decode(new Uint16Array(new Uint8Array([0x61, 0x62]).buffer)), "ab");
  assert.throws(() => new TextDecoder().decode("abc" as unknown as Uint8Array), TypeError);
});

test("encodeInto writes whole characters that fit", (assert) => {
  const out = new Uint8Array(5);
  assert.deepEqual(new TextEncoder().encodeInto("aあい", out), { read: 2, written: 4 });
  assert.deepEqual(list(out), [0x61, 0xe3, 0x81, 0x82, 0]);
  const four = new Uint8Array(4);
  assert.deepEqual(new TextEncoder().encodeInto("\u{1f600}", four), { read: 2, written: 4 }, "a pair reads two");
  assert.deepEqual(new TextEncoder().encodeInto("\u{1f600}", new Uint8Array(3)), { read: 0, written: 0 });
});

test("only UTF-8's labels are taken", (assert) => {
  for (const label of ["utf-8", "UTF8", " unicode-1-1-utf-8 ", "Unicode20UTF8"]) {
    assert.equal(new TextDecoder(label).encoding, "utf-8", label);
  }
  assert.throws(() => new TextDecoder("latin1"), RangeError);
  assert.throws(() => new TextDecoder("utf-16le"), RangeError);
});
