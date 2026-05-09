"""
Slice 1 smoke test: POST a hardcoded FLUX-schnell graph to a running ComfyUI,
wait for execution, fetch the generated image, save it locally.

Usage:
    python smoketest.py "your prompt here" [--seed N] [--out DIR]

Verifies our understanding of the ComfyUI HTTP + WS contract before we port
this to the workflow's Rust backend.
"""

import argparse
import json
import random
import sys
import time
import uuid
from pathlib import Path
from urllib import request, error

COMFY_URL = "http://127.0.0.1:8188"
CHECKPOINT = "flux1-schnell-fp8.safetensors"


def build_graph(prompt: str, seed: int, width: int = 1024, height: int = 1024) -> dict:
    """FLUX schnell: 4 steps, cfg=1.0, euler/simple, SD3-style empty latent."""
    return {
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "seed": seed,
                "steps": 4,
                "cfg": 1.0,
                "sampler_name": "euler",
                "scheduler": "simple",
                "denoise": 1.0,
                "model": ["4", 0],
                "positive": ["6", 0],
                "negative": ["7", 0],
                "latent_image": ["5", 0],
            },
        },
        "4": {
            "class_type": "CheckpointLoaderSimple",
            "inputs": {"ckpt_name": CHECKPOINT},
        },
        "5": {
            "class_type": "EmptySD3LatentImage",
            "inputs": {"width": width, "height": height, "batch_size": 1},
        },
        "6": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": prompt, "clip": ["4", 1]},
        },
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": "", "clip": ["4", 1]},
        },
        "8": {
            "class_type": "VAEDecode",
            "inputs": {"samples": ["3", 0], "vae": ["4", 2]},
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": {"images": ["8", 0], "filename_prefix": "lutin_smoketest"},
        },
    }


def http_json(method: str, path: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    req = request.Request(
        f"{COMFY_URL}{path}",
        data=data,
        method=method,
        headers={"Content-Type": "application/json"} if data else {},
    )
    with request.urlopen(req) as resp:
        return json.loads(resp.read())


def http_bytes(path: str) -> bytes:
    with request.urlopen(f"{COMFY_URL}{path}") as resp:
        return resp.read()


def queue_prompt(graph: dict, client_id: str) -> str:
    resp = http_json("POST", "/prompt", {"prompt": graph, "client_id": client_id})
    return resp["prompt_id"]


def wait_for(prompt_id: str, timeout_s: float = 300.0) -> dict:
    """Poll /history until the prompt_id appears with status set."""
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        hist = http_json("GET", f"/history/{prompt_id}")
        if prompt_id in hist:
            entry = hist[prompt_id]
            status = entry.get("status", {})
            if status.get("completed") or status.get("status_str") == "error":
                return entry
        time.sleep(0.5)
    raise TimeoutError(f"prompt {prompt_id} did not finish within {timeout_s}s")


def collect_outputs(entry: dict) -> list[dict]:
    """Flatten all 'images' entries across nodes in the history outputs."""
    out = []
    for _node_id, node_out in entry.get("outputs", {}).items():
        for img in node_out.get("images", []) or []:
            out.append(img)
    return out


def fetch_image(meta: dict) -> bytes:
    fn = meta["filename"]
    sub = meta.get("subfolder", "")
    typ = meta.get("type", "output")
    return http_bytes(f"/view?filename={fn}&subfolder={sub}&type={typ}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("prompt")
    ap.add_argument("--seed", type=int, default=None)
    ap.add_argument("--width", type=int, default=1024)
    ap.add_argument("--height", type=int, default=1024)
    ap.add_argument("--out", type=Path, default=Path("./out"))
    args = ap.parse_args()

    seed = args.seed if args.seed is not None else random.randint(0, 2**31 - 1)
    client_id = str(uuid.uuid4())
    args.out.mkdir(parents=True, exist_ok=True)

    print(f"client_id={client_id} seed={seed}")
    graph = build_graph(args.prompt, seed, args.width, args.height)

    t0 = time.monotonic()
    try:
        prompt_id = queue_prompt(graph, client_id)
    except error.HTTPError as e:
        print(f"POST /prompt failed: {e.code} {e.reason}", file=sys.stderr)
        print(e.read().decode(errors="replace"), file=sys.stderr)
        return 1
    print(f"queued prompt_id={prompt_id}")

    entry = wait_for(prompt_id)
    elapsed = time.monotonic() - t0

    status = entry.get("status", {})
    if status.get("status_str") == "error" or not status.get("completed"):
        print(f"execution error: {json.dumps(status, indent=2)}", file=sys.stderr)
        return 2

    images = collect_outputs(entry)
    if not images:
        print("no images in outputs", file=sys.stderr)
        return 3

    for i, meta in enumerate(images):
        bytes_ = fetch_image(meta)
        dest = args.out / f"{prompt_id}-{i}-seed{seed}.png"
        dest.write_bytes(bytes_)
        print(f"saved {dest} ({len(bytes_)} bytes)")

    print(f"done in {elapsed:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
