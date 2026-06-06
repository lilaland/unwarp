#!/usr/bin/env python3
"""Slice the narwhal reference sheet into per-pose frame PNGs.

Usage:
    python3 script/slice_sprites.py [--size 64] [--fps 8]

The reference sheet lives at assets/sprites/narwhal/reference/poses.png.
It is a horizontal strip: each pose occupies a column of N×64 tiles (where
N is the frame count for that pose). Poses are ordered left-to-right in the
same order as POSES below.

Each pose's frames are written to assets/sprites/narwhal/<pose>/frames/NN.png
and the first frame is also copied to assets/sprites/narwhal/<pose>/static.png.
meta.json is updated with the final frame list.
"""

import argparse
import json
import math
import shutil
import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    sys.exit("Pillow is required: pip install Pillow")

POSES = ["happy", "thinking", "waving", "fishing", "chatting"]

REPO_ROOT = Path(__file__).parent.parent
SPRITES_DIR = REPO_ROOT / "assets" / "sprites" / "narwhal"
REFERENCE_SHEET = SPRITES_DIR / "reference" / "poses.png"


def slice_sheet(size: int = 64, fps: int = 8) -> None:
    if not REFERENCE_SHEET.exists():
        sys.exit(f"Reference sheet not found: {REFERENCE_SHEET}")

    sheet = Image.open(REFERENCE_SHEET).convert("RGBA")
    sheet_w, sheet_h = sheet.size

    # Each pose gets an equal horizontal slice.
    # Frames stack vertically within each pose's column.
    pose_w = sheet_w // len(POSES)
    n_frames = sheet_h // size

    print(f"Sheet: {sheet_w}×{sheet_h}  |  pose_w={pose_w}  |  frames per pose={n_frames}")

    for i, pose in enumerate(POSES):
        pose_dir = SPRITES_DIR / pose
        frames_dir = pose_dir / "frames"
        frames_dir.mkdir(parents=True, exist_ok=True)

        frame_paths: list[str] = []
        x_offset = i * pose_w

        for f in range(n_frames):
            y_offset = f * size
            box = (x_offset, y_offset, x_offset + size, y_offset + size)
            frame_img = sheet.crop(box)
            out_path = frames_dir / f"{f:02d}.png"
            frame_img.save(out_path, "PNG")
            frame_paths.append(f"frames/{f:02d}.png")

        # static.png = first frame
        static_src = frames_dir / "00.png"
        if static_src.exists():
            shutil.copy(static_src, pose_dir / "static.png")

        # Update meta.json
        meta_path = pose_dir / "meta.json"
        if meta_path.exists():
            with open(meta_path) as fh:
                meta = json.load(fh)
        else:
            meta = {"pose": pose, "loop_mode": "forward"}

        meta["canvas_size"] = size
        meta["fps"] = fps
        meta["frames"] = frame_paths

        with open(meta_path, "w") as fh:
            json.dump(meta, fh, indent=2)
            fh.write("\n")

        print(f"  {pose}: {n_frames} frame(s) → {frames_dir}")

    print("Done.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--size", type=int, default=64, help="Canvas size in pixels (default 64)")
    parser.add_argument("--fps", type=int, default=8, help="Frames per second (default 8)")
    args = parser.parse_args()
    slice_sheet(size=args.size, fps=args.fps)
