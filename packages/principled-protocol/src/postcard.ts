// Minimal postcard subset — just the primitives chat's protocol needs.
// Postcard's standard flavour is LEB128 varints for unsigned integers
// (zigzag for signed), 1-byte bool, length-prefixed strings/Vec, 1-byte
// Option tag, varint(u32) discriminant for enums, fields in declared
// order for structs. Keep this file generic; chat-specific encoders
// live in `chat.ts`.

export class Writer {
  private buf: number[] = [];
  byte(b: number): void {
    this.buf.push(b & 0xff);
  }
  bytes(bs: Uint8Array): void {
    for (const b of bs) this.buf.push(b);
  }
  finish(): Uint8Array {
    return new Uint8Array(this.buf);
  }
}

export class Reader {
  private pos = 0;
  constructor(private readonly buf: Uint8Array) {}
  byte(): number {
    if (this.pos >= this.buf.length) throw new Error("postcard: unexpected EOF");
    return this.buf[this.pos++];
  }
  bytes(n: number): Uint8Array {
    if (this.pos + n > this.buf.length) throw new Error("postcard: unexpected EOF");
    const out = this.buf.subarray(this.pos, this.pos + n);
    this.pos += n;
    return out;
  }
  remaining(): number {
    return this.buf.length - this.pos;
  }
}

const enc = new TextEncoder();
const dec = new TextDecoder();

export function writeVarint(w: Writer, value: bigint): void {
  if (value < 0n) throw new Error("postcard: negative varint");
  let v = value;
  while (v >= 0x80n) {
    w.byte(Number(v & 0x7fn) | 0x80);
    v >>= 7n;
  }
  w.byte(Number(v));
}

// `maxBytes` matches postcard's per-width caps: 5 for u32, 10 for u64.
export function readVarint(r: Reader, maxBytes: number): bigint {
  let v = 0n;
  let shift = 0n;
  for (let i = 0; i < maxBytes; i++) {
    const b = r.byte();
    v |= BigInt(b & 0x7f) << shift;
    if ((b & 0x80) === 0) return v;
    shift += 7n;
  }
  throw new Error("postcard: varint too long");
}

export function writeU32(w: Writer, value: number): void {
  writeVarint(w, BigInt(value >>> 0));
}

export function readU32(r: Reader): number {
  return Number(readVarint(r, 5));
}

export function writeU64(w: Writer, value: bigint): void {
  writeVarint(w, value);
}

export function readU64(r: Reader): bigint {
  return readVarint(r, 10);
}

export function writeString(w: Writer, s: string): void {
  const bytes = enc.encode(s);
  writeVarint(w, BigInt(bytes.length));
  w.bytes(bytes);
}

export function readString(r: Reader): string {
  const len = Number(readVarint(r, 10));
  return dec.decode(r.bytes(len));
}

export function writeBool(w: Writer, b: boolean): void {
  w.byte(b ? 1 : 0);
}

export function readBool(r: Reader): boolean {
  const b = r.byte();
  if (b !== 0 && b !== 1) throw new Error(`postcard: invalid bool ${b}`);
  return b === 1;
}

export function writeOption<T>(
  w: Writer,
  v: T | null | undefined,
  inner: (w: Writer, x: T) => void,
): void {
  if (v === null || v === undefined) {
    w.byte(0);
  } else {
    w.byte(1);
    inner(w, v);
  }
}

export function readOption<T>(r: Reader, inner: (r: Reader) => T): T | null {
  const tag = r.byte();
  if (tag === 0) return null;
  if (tag === 1) return inner(r);
  throw new Error(`postcard: invalid Option tag ${tag}`);
}

export function writeVec<T>(
  w: Writer,
  items: readonly T[],
  inner: (w: Writer, x: T) => void,
): void {
  writeVarint(w, BigInt(items.length));
  for (const x of items) inner(w, x);
}

export function readVec<T>(r: Reader, inner: (r: Reader) => T): T[] {
  const len = Number(readVarint(r, 10));
  const out: T[] = new Array(len);
  for (let i = 0; i < len; i++) out[i] = inner(r);
  return out;
}

export const writeVariant = writeU32;
export const readVariant = readU32;
