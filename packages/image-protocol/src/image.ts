// Image protocol postcard bindings. Mirrors the Rust types in
// `workflows/image/src/lib.rs`. Variant indices follow declared
// order; keep this file in sync if engine-side enums grow new arms.

import * as pc from "./postcard";

export interface GenerateParams {
  prompt: string;
  /** When null, the engine picks a random u64. */
  seed: bigint | null;
  width: number;
  height: number;
}

export type ImageRequest = { kind: "generate"; params: GenerateParams };

export interface GeneratedImage {
  /** e.g. "image/png". */
  mime: string;
  /** Base64-encoded image bytes. Iframe renders via `data:` URL. */
  bytesB64: string;
  /** The seed actually used (echoed back so the UI can display it). */
  seed: bigint;
  /** Wall-clock ms spent in the engine for this generation. */
  ms: number;
}

export type ImageOk = { kind: "image"; image: GeneratedImage };

export type ImageError =
  | { kind: "comfyUnreachable"; reason: string }
  | { kind: "comfy"; reason: string }
  | { kind: "internal"; message: string };

/** Postcard-encoded `Result<ImageOk, ImageError>`. */
export type ImageResponse =
  | { ok: true; value: ImageOk }
  | { ok: false; error: ImageError };

export function encodeImageRequest(req: ImageRequest): Uint8Array {
  const w = new pc.Writer();
  switch (req.kind) {
    case "generate":
      pc.writeVariant(w, 0);
      pc.writeString(w, req.params.prompt);
      pc.writeOption(w, req.params.seed, (w, s) => pc.writeU64(w, s));
      pc.writeU32(w, req.params.width);
      pc.writeU32(w, req.params.height);
      break;
  }
  return w.finish();
}

export function decodeImageResponse(bytes: Uint8Array): ImageResponse {
  const r = new pc.Reader(bytes);
  const tag = pc.readVariant(r);
  if (tag === 0) return { ok: true, value: readImageOk(r) };
  if (tag === 1) return { ok: false, error: readImageError(r) };
  throw new Error(`postcard: invalid Result tag ${tag}`);
}

function readImageOk(r: pc.Reader): ImageOk {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return {
        kind: "image",
        image: {
          mime: pc.readString(r),
          bytesB64: pc.readString(r),
          seed: pc.readU64(r),
          ms: pc.readU32(r),
        },
      };
    default:
      throw new Error(`postcard: invalid ImageOk ${v}`);
  }
}

function readImageError(r: pc.Reader): ImageError {
  const v = pc.readVariant(r);
  switch (v) {
    case 0:
      return { kind: "comfyUnreachable", reason: pc.readString(r) };
    case 1:
      return { kind: "comfy", reason: pc.readString(r) };
    case 2:
      return { kind: "internal", message: pc.readString(r) };
    default:
      throw new Error(`postcard: invalid ImageError ${v}`);
  }
}

export function imageErrorMessage(err: ImageError): string {
  switch (err.kind) {
    case "comfyUnreachable":
      return `ComfyUI unreachable: ${err.reason}`;
    case "comfy":
      return `ComfyUI error: ${err.reason}`;
    case "internal":
      return `internal: ${err.message}`;
  }
}
