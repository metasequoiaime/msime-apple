#!/usr/bin/env python3
"""Keep Harmony's NAPI provider-frame bridge bounded before native copies.

The ArkTS side can hand native code an arbitrary ArrayBuffer.  Doubao frames are
bounded by the shared one-megabyte wire contract, so the bridge must inspect the
byte length before copying it into a second native allocation.
"""

from pathlib import Path


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    source = (root / "platforms/harmony/native/client_napi.cpp").read_text(encoding="utf-8")
    helper_start = source.index(
        "static bool argumentArrayBuffer(napi_env env, napi_value value,"
    )
    helper_end = source.index("\n}\n\nstatic bool gzipCompress", helper_start)
    helper = source[helper_start:helper_end]
    checks = {
        "length is checked before native copy": helper.index("if (length > maximum) return false;")
        < helper.index("out.assign("),
        "encode passes the payload limit": "argumentArrayBuffer(env, argv[3], payload, kMaxFramePayload)"
        in source,
        "decode passes the frame limit": "argumentArrayBuffer(env, argv[0], frame, kMaxFrameBytes)"
        in source,
    }
    missing = [name for name, present in checks.items() if not present]
    if missing:
        print("harmony NAPI frame bounds: missing " + ", ".join(missing))
        return 1
    print("harmony NAPI frame bounds: provider ArrayBuffers are bounded before copying")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
