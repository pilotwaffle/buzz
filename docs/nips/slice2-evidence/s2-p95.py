#!/usr/bin/env python3
"""s2-p95.py — nearest-rank p95 latency computer for Slice 2 AC-17.

Usage:
    python s2-p95.py <logfile>

The logfile contains one integer per line (latency in milliseconds).
Prints: filename, p95 (ms), n, min, max, mean.
"""

import sys
import math
from pathlib import Path


def nearest_rank_p95(samples: list[int]) -> int:
    """p95 via nearest-rank: sort ascending, pick ceil(0.95 * n) - 1."""
    n = len(samples)
    if n == 0:
        raise ValueError("no samples")
    sorted_samples = sorted(samples)
    idx = math.ceil(0.95 * n) - 1
    return sorted_samples[idx]


def main() -> None:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <logfile>", file=sys.stderr)
        sys.exit(2)

    path = Path(sys.argv[1])
    if not path.is_file():
        print(f"Error: {path} is not a file", file=sys.stderr)
        sys.exit(1)

    samples: list[int] = []
    with path.open() as f:
        for line in f:
            stripped = line.strip()
            if not stripped:
                continue
            try:
                samples.append(int(stripped))
            except ValueError:
                print(f"Warning: skipping non-integer line: {stripped!r}", file=sys.stderr)

    if not samples:
        print("Error: no valid samples found", file=sys.stderr)
        sys.exit(1)

    n = len(samples)
    p95 = nearest_rank_p95(samples)
    min_val = min(samples)
    max_val = max(samples)
    mean_val = sum(samples) / n

    print(f"file: {path.name}")
    print(f"p95:  {p95} ms")
    print(f"n:    {n}")
    print(f"min:  {min_val} ms")
    print(f"max:  {max_val} ms")
    print(f"mean: {mean_val:.1f} ms")


if __name__ == "__main__":
    main()