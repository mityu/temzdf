// TextEncoder and TextDecoder (the WHATWG Encoding Standard's UTF-8; QuickJS
// has neither), installed as globals by the prelude. The byte work is
// native (`utf8Encode`, `utf8Decode`: src/text_codec.rs); the classes keep
// a stream's state here.

/** The prelude's native functions these build on. */
export type Utf8 = {
  /** A well-formed string's UTF-8 bytes. */
  utf8Encode(text: string): Uint8Array;
  /** `bytes` as text (throws a TypeError when `fatal` and invalid) and
   * how many bytes at the end it left undecoded (`stream`: incomplete). */
  utf8Decode(bytes: Uint8Array, fatal: boolean, stream: boolean): [string, number];
};

/** The labels naming UTF-8 (the standard's list). */
const UTF8_LABELS = ["unicode-1-1-utf-8", "unicode11utf8", "unicode20utf8", "utf-8", "utf8", "x-unicode20utf8"];

/** `input` as bytes, without copying (an ArrayBuffer, a typed array or a DataView). */
function bytesOf(input: BufferSource | undefined): Uint8Array {
  if (input === undefined) return new Uint8Array(0);
  if (input instanceof ArrayBuffer || (typeof SharedArrayBuffer === "function" && input instanceof SharedArrayBuffer)) {
    return new Uint8Array(input);
  }
  if (ArrayBuffer.isView(input)) return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  throw new TypeError("The provided value is not of type '(ArrayBuffer or ArrayBufferView)'");
}

/** What can be decoded. */
type BufferSource = ArrayBufferLike | ArrayBufferView;

/** `text` with lone surrogates as U+FFFD (ES2024; the prelude's lib is ES2022). */
const wellFormed = (text: string): string => (text as unknown as { toWellFormed(): string }).toWellFormed();

/** UTF-8 bytes per code point of a well-formed string's character. */
const utf8Length = (cp: number): number => (cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4);

export function install(g: Record<string, unknown>, utf8: Utf8): void {
  class TextEncoder {
    get encoding(): string {
      return "utf-8";
    }

    encode(input = ""): Uint8Array {
      // Lone surrogates become U+FFFD, as the standard says.
      return utf8.utf8Encode(wellFormed(String(input)));
    }

    encodeInto(source: string, destination: Uint8Array): { read: number; written: number } {
      if (!(destination instanceof Uint8Array)) throw new TypeError("The destination is not a Uint8Array");
      const text = wellFormed(String(source));
      // Whole characters only, as many as fit.
      let read = 0;
      let written = 0;
      for (const ch of text) {
        const n = utf8Length(ch.codePointAt(0) as number);
        if (written + n > destination.length) break;
        written += n;
        read += ch.length;
      }
      destination.set(utf8.utf8Encode(text.slice(0, read)));
      return { read, written };
    }
  }

  class TextDecoder {
    readonly #fatal: boolean;
    readonly #ignoreBOM: boolean;
    /** Bytes of an incomplete sequence a streaming decode left. */
    #held: Uint8Array = new Uint8Array(0);
    /** Whether a BOM at what comes next is the stream's (and dropped). */
    #atStart = true;

    constructor(label = "utf-8", options: { fatal?: boolean; ignoreBOM?: boolean } = {}) {
      if (!UTF8_LABELS.includes(String(label).trim().toLowerCase())) {
        throw new RangeError(`The encoding label provided ('${label}') is invalid or not supported.`);
      }
      this.#fatal = Boolean(options?.fatal);
      this.#ignoreBOM = Boolean(options?.ignoreBOM);
    }

    get encoding(): string {
      return "utf-8";
    }

    get fatal(): boolean {
      return this.#fatal;
    }

    get ignoreBOM(): boolean {
      return this.#ignoreBOM;
    }

    decode(input?: BufferSource, options: { stream?: boolean } = {}): string {
      const stream = Boolean(options?.stream);
      let bytes = bytesOf(input);
      if (this.#held.length > 0) {
        const joined = new Uint8Array(this.#held.length + bytes.length);
        joined.set(this.#held);
        joined.set(bytes, this.#held.length);
        bytes = joined;
      }
      let text: string;
      let keep: number;
      try {
        [text, keep] = utf8.utf8Decode(bytes, this.#fatal, stream);
      } catch (e) {
        // A failed decode starts the stream over, as the standard says.
        this.#held = new Uint8Array(0);
        this.#atStart = true;
        throw e;
      }
      this.#held = bytes.slice(bytes.length - keep);
      if (this.#atStart && text.length > 0) {
        if (!this.#ignoreBOM && text.charCodeAt(0) === 0xfeff) text = text.slice(1);
        this.#atStart = false;
      }
      if (!stream) {
        this.#held = new Uint8Array(0);
        this.#atStart = true;
      }
      return text;
    }
  }

  for (const [name, value] of [
    ["TextEncoder", TextEncoder],
    ["TextDecoder", TextDecoder],
  ] as const) {
    Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
  }
}
