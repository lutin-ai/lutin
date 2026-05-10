// Image protocol postcard bindings. Mirrors the Rust types in
// `workflows/image/src/lib.rs`. Variant indices follow declared
// order; keep this file in sync if engine-side enums grow new arms.

import * as pc from "./postcard";

export interface ImageSettings {
  comfyuiUrl: string;
  defaultWidth: number;
  defaultHeight: number;
  defaultCount: number;
  defaultSteps: number;
  defaultCfg: number;
  /** Default graph builder. See `MODEL_IDS`. */
  defaultModelId: string;
}

export const MODEL_FLUX_SCHNELL = "flux-schnell";
export const MODEL_FLUX2_DEV = "flux2-dev";
export const MODEL_IDS = [MODEL_FLUX_SCHNELL, MODEL_FLUX2_DEV] as const;

/** Per-model recommended defaults. The composer uses these to pick
 *  field placeholders so a user switching models doesn't have to
 *  remember that schnell wants 4 steps but flux2-dev wants ~28. */
export interface ModelDefaults {
  steps: number;
  cfg: number;
  label: string;
}
export const MODEL_DEFAULTS: Record<string, ModelDefaults> = {
  [MODEL_FLUX_SCHNELL]: { steps: 4, cfg: 1.0, label: "FLUX schnell" },
  [MODEL_FLUX2_DEV]: { steps: 28, cfg: 3.5, label: "FLUX.2 dev" },
};

export interface GenerateParams {
  prompt: string;
  negativePrompt: string;
  /** When null, the engine picks a random u64. */
  seed: bigint | null;
  width: number;
  height: number;
  count: number;
  steps: number;
  /** KSampler cfg for schnell; FluxGuidance guidance for flux2-dev. */
  cfg: number;
  modelId: string;
}

export type ImageRequest =
  | { kind: "generate"; params: GenerateParams }
  | { kind: "getSettings" }
  | { kind: "setSettings"; settings: ImageSettings }
  | { kind: "healthCheck" }
  | { kind: "loadTranscript" }
  | { kind: "getImage"; imageId: string };

export interface GeneratedImage {
  /** Path relative to the session state dir; canonical reference for
   *  re-fetching this image via `getImage` after a session restore. */
  imageId: string;
  /** e.g. "image/png". */
  mime: string;
  /** Base64-encoded image bytes. Iframe renders via `data:` URL. */
  bytesB64: string;
  /** The seed actually used (echoed back so the UI can display it). */
  seed: bigint;
  /** Wall-clock ms spent in the engine for this generation. */
  ms: number;
}

export type TranscriptStatus =
  | { kind: "done"; images: TranscriptImage[] }
  | { kind: "error"; message: string };

export interface TranscriptImage {
  imageId: string;
  mime: string;
  seed: bigint;
  ms: number;
}

export interface TranscriptEntry {
  prompt: string;
  negativePrompt: string;
  width: number;
  height: number;
  steps: number;
  cfg: number;
  /** RFC3339. */
  startedAt: string;
  status: TranscriptStatus;
  modelId: string;
}

export type ImageOk =
  | { kind: "images"; images: GeneratedImage[] }
  | { kind: "settings"; settings: ImageSettings }
  | { kind: "settingsUpdated" }
  | { kind: "health"; reachable: boolean; message: string }
  | { kind: "transcript"; entries: TranscriptEntry[] }
  | { kind: "image"; image: GeneratedImage };

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
      writeGenerateParams(w, req.params);
      break;
    case "getSettings":
      pc.writeVariant(w, 1);
      break;
    case "setSettings":
      pc.writeVariant(w, 2);
      writeSettings(w, req.settings);
      break;
    case "healthCheck":
      pc.writeVariant(w, 3);
      break;
    case "loadTranscript":
      pc.writeVariant(w, 4);
      break;
    case "getImage":
      pc.writeVariant(w, 5);
      pc.writeString(w, req.imageId);
      break;
  }
  return w.finish();
}

function writeGenerateParams(w: pc.Writer, p: GenerateParams): void {
  pc.writeString(w, p.prompt);
  pc.writeString(w, p.negativePrompt);
  pc.writeOption(w, p.seed, (w, s) => pc.writeU64(w, s));
  pc.writeU32(w, p.width);
  pc.writeU32(w, p.height);
  pc.writeU32(w, p.count);
  pc.writeU32(w, p.steps);
  writeF32(w, p.cfg);
  pc.writeString(w, p.modelId);
}

function writeSettings(w: pc.Writer, s: ImageSettings): void {
  pc.writeString(w, s.comfyuiUrl);
  pc.writeU32(w, s.defaultWidth);
  pc.writeU32(w, s.defaultHeight);
  pc.writeU32(w, s.defaultCount);
  pc.writeU32(w, s.defaultSteps);
  writeF32(w, s.defaultCfg);
  pc.writeString(w, s.defaultModelId);
}

function readSettings(r: pc.Reader): ImageSettings {
  return {
    comfyuiUrl: pc.readString(r),
    defaultWidth: pc.readU32(r),
    defaultHeight: pc.readU32(r),
    defaultCount: pc.readU32(r),
    defaultSteps: pc.readU32(r),
    defaultCfg: readF32(r),
    defaultModelId: pc.readString(r),
  };
}

// Postcard's standard flavour writes f32 as little-endian raw 4 bytes.
function writeF32(w: pc.Writer, value: number): void {
  const buf = new ArrayBuffer(4);
  new DataView(buf).setFloat32(0, value, true);
  w.bytes(new Uint8Array(buf));
}

function readF32(r: pc.Reader): number {
  const bs = r.bytes(4);
  // Copy into a fresh buffer because `bs` is a subarray view; DataView
  // on the original would respect its byteOffset and is fine, but
  // copying keeps the call shape symmetric with `writeF32`.
  const buf = new ArrayBuffer(4);
  new Uint8Array(buf).set(bs);
  return new DataView(buf).getFloat32(0, true);
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
    case 0: {
      const images = pc.readVec(r, readGeneratedImage);
      return { kind: "images", images };
    }
    case 1:
      return { kind: "settings", settings: readSettings(r) };
    case 2:
      return { kind: "settingsUpdated" };
    case 3:
      return {
        kind: "health",
        reachable: pc.readBool(r),
        message: pc.readString(r),
      };
    case 4: {
      const entries = pc.readVec(r, readTranscriptEntry);
      return { kind: "transcript", entries };
    }
    case 5:
      return { kind: "image", image: readGeneratedImage(r) };
    default:
      throw new Error(`postcard: invalid ImageOk ${v}`);
  }
}

function readGeneratedImage(r: pc.Reader): GeneratedImage {
  return {
    imageId: pc.readString(r),
    mime: pc.readString(r),
    bytesB64: pc.readString(r),
    seed: pc.readU64(r),
    ms: pc.readU32(r),
  };
}

function readTranscriptEntry(r: pc.Reader): TranscriptEntry {
  return {
    prompt: pc.readString(r),
    negativePrompt: pc.readString(r),
    width: pc.readU32(r),
    height: pc.readU32(r),
    steps: pc.readU32(r),
    cfg: readF32(r),
    startedAt: pc.readString(r),
    status: readTranscriptStatus(r),
    modelId: pc.readString(r),
  };
}

function readTranscriptStatus(r: pc.Reader): TranscriptStatus {
  const v = pc.readVariant(r);
  switch (v) {
    case 0: {
      const images = pc.readVec(r, readTranscriptImage);
      return { kind: "done", images };
    }
    case 1:
      return { kind: "error", message: pc.readString(r) };
    default:
      throw new Error(`postcard: invalid TranscriptStatus ${v}`);
  }
}

function readTranscriptImage(r: pc.Reader): TranscriptImage {
  return {
    imageId: pc.readString(r),
    mime: pc.readString(r),
    seed: pc.readU64(r),
    ms: pc.readU32(r),
  };
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

/** Server-pushed events streamed alongside the request/response channel.
 *  Mirrors `ImageEvent` in `workflows/image/src/lib.rs`. */
export type ImageEvent =
  | { kind: "jobQueued"; jobId: string }
  | { kind: "jobProgress"; jobId: string; step: number; total: number }
  | { kind: "jobDone"; jobId: string }
  | { kind: "jobError"; jobId: string; message: string };

export function decodeImageEvent(bytes: Uint8Array): ImageEvent {
  const r = new pc.Reader(bytes);
  const tag = pc.readVariant(r);
  switch (tag) {
    case 0:
      return { kind: "jobQueued", jobId: pc.readString(r) };
    case 1:
      return {
        kind: "jobProgress",
        jobId: pc.readString(r),
        step: pc.readU32(r),
        total: pc.readU32(r),
      };
    case 2:
      return { kind: "jobDone", jobId: pc.readString(r) };
    case 3:
      return {
        kind: "jobError",
        jobId: pc.readString(r),
        message: pc.readString(r),
      };
    default:
      throw new Error(`postcard: invalid ImageEvent tag ${tag}`);
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
