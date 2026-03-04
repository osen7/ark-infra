#!/usr/bin/env python3
"""RDMA mock probe: stream JSONL events from a fixture file."""

import argparse
import json
import os
import sys
import time


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Ark RDMA mock probe")
    parser.add_argument(
        "--file",
        default=os.environ.get(
            "ARK_RDMA_MOCK_FILE", "examples/mock/rdma/events-pfc-storm.jsonl"
        ),
        help="Path to JSONL fixture file",
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=float(os.environ.get("ARK_RDMA_MOCK_INTERVAL", "0.5")),
        help="Seconds between emitted lines",
    )
    parser.add_argument(
        "--loop",
        action="store_true",
        default=os.environ.get("ARK_RDMA_MOCK_LOOP", "0") in ("1", "true", "TRUE"),
        help="Replay the fixture file in a loop",
    )
    parser.add_argument(
        "--refresh-ts",
        action="store_true",
        default=os.environ.get("ARK_RDMA_MOCK_REFRESH_TS", "1") in ("1", "true", "TRUE"),
        help="Rewrite ts to current time when emitting events",
    )
    return parser.parse_args()


def stream_file(path: str, interval: float, refresh_ts: bool) -> bool:
    emitted = False
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            out = line
            if refresh_ts:
                try:
                    payload = json.loads(line)
                    payload["ts"] = int(time.time() * 1000)
                    out = json.dumps(payload, ensure_ascii=False)
                except Exception:
                    out = line
            print(out, flush=True)
            emitted = True
            if interval > 0:
                time.sleep(interval)
    return emitted


def main() -> int:
    args = parse_args()
    while True:
        try:
            emitted = stream_file(args.file, args.interval, args.refresh_ts)
            if not emitted:
                print(f"[rdma-mock] fixture is empty: {args.file}", file=sys.stderr)
            if not args.loop:
                return 0
        except FileNotFoundError:
            print(f"[rdma-mock] fixture not found: {args.file}", file=sys.stderr)
            return 1
        except BrokenPipeError:
            return 0
        except KeyboardInterrupt:
            return 0
        except Exception as exc:
            print(f"[rdma-mock] unexpected error: {exc}", file=sys.stderr)
            return 1


if __name__ == "__main__":
    raise SystemExit(main())
