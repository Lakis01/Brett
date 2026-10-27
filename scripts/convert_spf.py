#!/usr/bin/env python3
"""
Convert Scid Photo File (Players.spf) to Brett's zero-copy binary pack (photos.bin).

The input comes from Scid's Player_photos.zip (scid/PlayerData/Player_photos.zip).
That archive has no explicit redistribution licence, so the generated pack is
for local use only and must not be committed or bundled with Brett. See
docs/PHOTOS.md and THIRD_PARTY_NOTICES.md.

Uses libwebp with maximum compression quality settings:
- method = 6 (exhaustive prediction analysis)
- pass = 10 (maximum 10 entropy-coding analysis passes)
- use_sharp_yuv = 1 (sharp RGB->YUV conversion for crisp thumbnail edges)
- segments = 4, sns_strength = 80 (maximum spatial noise shaping)
- alpha_filtering = 2 (best predictive filtering)

Memory-safe architecture:
- Streaming line-by-line parser
- Bounded input/output queues (maxsize=32)
- Multi-threaded C worker pool
- WebP stream written directly to disk
- Peak RAM usage strictly < 350 MB (safe from systemd-oomd)
"""

import argparse
import base64
import ctypes
import io
import os
import queue
import re
import struct
import subprocess
import sys
import threading
import time
from PIL import Image

C_HELPER_SOURCE = """
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <webp/encode.h>

size_t encode_webp_max_quality(const uint8_t* rgba, int width, int height, int stride, float quality, uint8_t** out_data) {
    WebPConfig config;
    if (!WebPConfigPreset(&config, WEBP_PRESET_PICTURE, quality)) return 0;
    config.method = 6;
    config.pass = 10;
    config.use_sharp_yuv = 1;
    config.segments = 4;
    config.sns_strength = 80;
    config.autofilter = 1;
    config.filter_type = 1;
    config.alpha_filtering = 2;
    config.alpha_quality = 85;

    WebPPicture pic;
    if (!WebPPictureInit(&pic)) return 0;
    pic.use_argb = 1;
    pic.width = width;
    pic.height = height;
    if (!WebPPictureImportRGBA(&pic, rgba, stride)) {
        WebPPictureFree(&pic);
        return 0;
    }

    WebPMemoryWriter writer;
    WebPMemoryWriterInit(&writer);
    pic.writer = WebPMemoryWrite;
    pic.custom_ptr = &writer;

    if (!WebPEncode(&config, &pic)) {
        WebPPictureFree(&pic);
        WebPMemoryWriterClear(&writer);
        return 0;
    }

    WebPPictureFree(&pic);
    *out_data = writer.mem;
    return writer.size;
}
"""

def ensure_c_helper(helper_path: str) -> ctypes.CDLL:
    if not os.path.exists(helper_path):
        cmd = ["gcc", "-shared", "-O3", "-fPIC", "-x", "c", "-", "-o", helper_path, "-lwebp"]
        subprocess.run(cmd, input=C_HELPER_SOURCE.encode(), check=True)
    lib = ctypes.CDLL(helper_path)
    lib.encode_webp_max_quality.argtypes = [
        ctypes.c_char_p, ctypes.c_int, ctypes.c_int, ctypes.c_int,
        ctypes.c_float, ctypes.POINTER(ctypes.c_void_p)
    ]
    lib.encode_webp_max_quality.restype = ctypes.c_size_t
    return lib

def player_key(name: str) -> str:
    """Matches Brett's src/players.rs player_key implementation exactly."""
    words = sorted(list(set(w.lower() for w in re.split(r"[^\w]+", name) if w)))
    return " ".join(words)

def main():
    parser = argparse.ArgumentParser(description="Convert Players.spf to photos.bin")
    parser.add_argument("--input", "-i", default="/home/gratis01/Downloads/Brett Resources/Photos/Players.spf",
                        help="Path to Players.spf")
    parser.add_argument("--output", "-o", default="/home/gratis01/Programs/Brett/assets/photos.bin",
                        help="Path to output photos.bin")
    parser.add_argument("--threads", "-t", type=int, default=8, help="Number of worker threads (default: 8)")
    parser.add_argument("--quality", "-q", type=float, default=76.0, help="WebP quality (default: 76.0, ~10%% size)")
    args = parser.parse_args()

    if not os.path.isfile(args.input):
        print(f"Error: Input file '{args.input}' not found.", file=sys.stderr)
        sys.exit(1)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    helper_so = os.path.join(os.path.dirname(os.path.abspath(__file__)), "libwebp_helper.so")
    webp_lib = ensure_c_helper(helper_so)
    c_free = ctypes.CDLL(None).free
    c_free.argtypes = [ctypes.c_void_p]

    in_queue = queue.Queue(maxsize=32)
    out_queue = queue.Queue(maxsize=32)

    def worker():
        while True:
            item = in_queue.get()
            if item is None:
                in_queue.task_done()
                break
            raw_name, b64_str = item
            key = player_key(raw_name)
            try:
                raw_bytes = base64.b64decode(b64_str)
                im = Image.open(io.BytesIO(raw_bytes)).convert("RGBA")
                out_p = ctypes.c_void_p()
                sz = webp_lib.encode_webp_max_quality(
                    im.tobytes(), im.width, im.height, im.width * 4,
                    ctypes.c_float(args.quality), ctypes.byref(out_p)
                )
                if sz > 0 and out_p.value:
                    webp_bytes = ctypes.string_at(out_p.value, sz)
                    c_free(out_p.value)
                    out_queue.put((key, len(raw_bytes), webp_bytes))
                else:
                    print(f"Warning: Failed to encode image for '{raw_name}'", file=sys.stderr)
            except Exception as e:
                print(f"Error processing '{raw_name}': {e}", file=sys.stderr)
            finally:
                in_queue.task_done()

    threads = [threading.Thread(target=worker, daemon=True) for _ in range(args.threads)]
    for t in threads:
        t.start()

    temp_data_path = args.output + ".tmp"
    data_file = open(temp_data_path, "wb")
    index = [] # list of (key, offset, length)

    def writer_loop():
        cur_offset = 0
        total_orig = 0
        total_webp = 0
        count = 0
        start_time = time.time()
        last_report = start_time

        while True:
            item = out_queue.get()
            if item is None:
                out_queue.task_done()
                break
            key, orig_len, webp_bytes = item
            webp_len = len(webp_bytes)
            data_file.write(webp_bytes)
            index.append((key, cur_offset, webp_len))
            cur_offset += webp_len
            total_orig += orig_len
            total_webp += webp_len
            count += 1
            out_queue.task_done()

            now = time.time()
            if now - last_report >= 3.0:
                elapsed = now - start_time
                speed = count / elapsed if elapsed > 0 else 0
                pct = (total_webp / total_orig * 100) if total_orig > 0 else 0
                print(f"[{count:5d}/14152] {speed:5.1f} img/s | WebP avg: {total_webp/count:.0f}B ({pct:.1f}%) | Elapsed: {elapsed/60:.1f}m", flush=True)
                last_report = now

        data_file.close()
        elapsed = time.time() - start_time
        pct = (total_webp / total_orig * 100) if total_orig > 0 else 0
        print(f"Encoding complete: {count} images in {elapsed/60:.2f} mins ({count/elapsed:.1f} img/s). Size ratio: {pct:.1f}%")

    writer_thread = threading.Thread(target=writer_loop)
    writer_thread.start()

    # Producer thread reading Players.spf line by line
    print(f"Starting conversion with {args.threads} threads (Quality={args.quality}, Method=6, Pass=10, SharpYUV=1)...")
    with open(args.input, "r", encoding="utf-8") as f:
        cur_name = None
        cur_b64 = []
        for line in f:
            line_s = line.strip()
            if line_s.startswith('photo "'):
                if cur_name and cur_b64:
                    in_queue.put((cur_name, "".join(cur_b64)))
                cur_name = line_s[7:-3]
                cur_b64 = []
            elif line_s != "}":
                cur_b64.append(line_s)
        if cur_name and cur_b64:
            in_queue.put((cur_name, "".join(cur_b64)))

    for _ in range(args.threads):
        in_queue.put(None)
    for t in threads:
        t.join()

    out_queue.put(None)
    writer_thread.join()

    # Sort index by key for binary search in Rust
    print("Sorting index and assembling final photos.bin...")
    index.sort(key=lambda x: x[0])

    # Index entry size: 1 byte len + key bytes + 4 bytes offset + 4 bytes length
    index_bytes_total = 8 + sum(1 + len(k.encode("utf-8")) + 8 for k, _, _ in index)

    with open(args.output, "wb") as final_f:
        # Header: magic b"BPF1" + count u32
        final_f.write(b"BPF1")
        final_f.write(struct.pack("<I", len(index)))

        # Write index table with adjusted offsets
        for key, offset, length in index:
            kb = key.encode("utf-8")
            final_f.write(struct.pack("<B", len(kb)))
            final_f.write(kb)
            final_f.write(struct.pack("<II", offset + index_bytes_total, length))

        # Stream-copy data from temp file
        with open(temp_data_path, "rb") as tmp_in:
            while True:
                chunk = tmp_in.read(1024 * 1024)
                if not chunk:
                    break
                final_f.write(chunk)

    os.remove(temp_data_path)
    final_size_mb = os.path.getsize(args.output) / (1024 * 1024)
    print(f"Successfully generated '{args.output}' ({final_size_mb:.2f} MB).")

if __name__ == "__main__":
    main()
