#!/usr/bin/env python3
"""Fetch Flash Station build lists and write the WebUI fingerprint template.

The template is the data the Select Fingerprint picker renders from. It is
refreshed from Google's Flash Station endpoint, either at build time or on
demand, so the shipped WebUI never has to wait on the network to open a picker.

This script is deliberately conservative about writing:

* A fetch is only written after every product returned usable data, so a partial
  response can never shrink the template.
* The new data is written to a temporary file in the same directory and then
  renamed over the target. ``rename`` is atomic on the same filesystem, so a
  reader either sees the old template or the new one, never a truncated file.
* If anything fails, the existing template is left exactly as it was and the
  script exits non-zero. The caller decides whether that is fatal.

Usage:
    scripts/fetch_fingerprint_template.py --output webui/src/fingerprint-template.json
    scripts/fetch_fingerprint_template.py --output ... --allow-partial
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import re
import sys
import tempfile
import urllib.error
import urllib.request

FLASH_SITE = "https://flash.android.com/"
BUILDS_API = "https://content-flashstation-pa.googleapis.com/v1/builds"
REFERER = FLASH_SITE
TIMEOUT = 30

# Kept in sync with BUILD_LETTER_MAJOR in the WebUI, which is what resolves an
# Android major for builds that publish neither versionName nor apiLevel.
BUILD_LETTER_MAJOR = {"S": 12, "T": 13, "U": 14, "A": 15, "B": 16, "C": 17}

# Devices declared in the WebUI. Parsed rather than duplicated so the template
# can never drift from the list the picker actually renders.
DEFAULT_CONSTANT = os.path.join("webui", "src", "constant.ts")
DEVICE_RE = re.compile(
    r"product:\s*'(?P<product>[^']+)',\s*model:\s*'(?P<model>[^']+)',"
    r"\s*min:\s*(?P<min>\d+),\s*max:\s*(?P<max>\d+)"
)


class TemplateError(RuntimeError):
    """Raised when the fetched data cannot be trusted enough to write."""


def parse_devices(constant_path: str) -> list[dict]:
    with open(constant_path, encoding="utf-8") as handle:
        source = handle.read()
    start = source.find("PIXEL_DEVICES")
    if start < 0:
        raise TemplateError(f"PIXEL_DEVICES not found in {constant_path}")
    end = source.find("\n]", start)
    if end < 0:
        raise TemplateError("unterminated PIXEL_DEVICES declaration")
    devices = []
    for match in DEVICE_RE.finditer(source[start:end]):
        devices.append(
            {
                "product": match.group("product"),
                "model": match.group("model"),
                "min": int(match.group("min")),
                "max": int(match.group("max")),
            }
        )
    if not devices:
        raise TemplateError(f"no devices parsed from {constant_path}")
    return devices


def http_get(url: str, referer: str | None = None) -> str:
    request = urllib.request.Request(url, headers={"User-Agent": "OhMyKeymint"})
    if referer:
        request.add_header("Referer", referer)
    with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
        if response.status != 200:
            raise TemplateError(f"HTTP {response.status} for {url}")
        return response.read().decode("utf-8", errors="replace")


def fetch_api_key() -> str:
    """Scrape the public API key from flash.android.com."""
    html = http_get(FLASH_SITE)
    match = re.search(r"AIzaSy[A-Za-z0-9_-]{33}", html)
    if not match:
        raise TemplateError("no API key found on flash.android.com")
    return match.group(0)


def build_major(build: dict) -> int | None:
    """Resolve the Android major, mirroring the WebUI's buildMajor()."""
    version_name = build.get("versionName")
    if isinstance(version_name, str):
        match = re.search(r"(\d+)\s*$", version_name)
        if match:
            return int(match.group(1))
    track = (build.get("previewMetadata") or {}).get("releaseTrackName") or ""
    match = re.match(r"^Android (\d+)", track)
    if match:
        return int(match.group(1))
    api_level = build.get("apiLevel")
    if isinstance(api_level, int) and api_level > 0:
        return api_level - 20
    letter = (build.get("releaseCandidateName") or "")[:1].upper()
    return BUILD_LETTER_MAJOR.get(letter)


def extract_rows(device: dict, builds: list[dict]) -> list[list]:
    """Reduce one device's build list to the tuple the WebUI consumes."""
    rows = []
    for build in builds:
        if not isinstance(build, dict):
            continue
        # The picker only ever offers retail user builds, never factory images
        # or bootloader targets.
        if build.get("target") != f"{build.get('product')}-user":
            continue
        name = build.get("releaseCandidateName")
        incremental = build.get("buildId")
        if not name or not incremental:
            continue
        major = build_major(build)
        if major is None or not device["min"] <= major <= device["max"]:
            continue
        rows.append([name, str(incremental), major])
    # Newest major first, then newest build within the major, so the picker's
    # default selection lands on the latest build.
    rows.sort(key=lambda row: (row[2], row[0]), reverse=True)
    return rows


def fetch_product(device: dict, key: str) -> dict:
    url = f"{BUILDS_API}?product={device['product']}&key={key}"
    payload = json.loads(http_get(url, referer=REFERER))
    builds = payload.get("flashstationBuild")
    if not isinstance(builds, list):
        raise TemplateError(f"{device['product']}: no flashstationBuild array")
    return device["product"], extract_rows(device, builds)


def build_template(devices: list[dict], workers: int) -> dict:
    key = fetch_api_key()
    template: dict[str, dict] = {}
    failures: list[str] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(fetch_product, device, key): device for device in devices}
        for future in concurrent.futures.as_completed(futures):
            device = futures[future]
            try:
                product, rows = future.result()
            except Exception as error:  # noqa: BLE001 - reported, not raised
                failures.append(f"{device['product']}: {error}")
                continue
            if not rows:
                failures.append(f"{device['product']}: no usable builds")
                continue
            template[product] = {
                "model": device["model"],
                "min": device["min"],
                "max": device["max"],
                # Tuples keep the shipped payload small; the WebUI casts them
                # back to FlashBuild-shaped objects.
                "builds": rows,
            }
    if failures:
        raise TemplateError("; ".join(sorted(failures)))
    # Upstream response order varies between calls, and so does the order the
    # workers finish in. Sorting keeps the written file stable across runs, which
    # matters because a committed template that reshuffles on every build would
    # show as unrelated churn in review.
    return dict(sorted(template.items()))


def load_existing(path: str) -> dict | None:
    try:
        with open(path, encoding="utf-8") as handle:
            existing = json.load(handle)
    except (OSError, ValueError):
        return None
    return existing if isinstance(existing, dict) else None


def atomic_write(path: str, payload: str) -> None:
    """Write via a same-directory temp file and rename, so readers never tear."""
    directory = os.path.dirname(os.path.abspath(path)) or "."
    os.makedirs(directory, exist_ok=True)
    handle = tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=directory,
        prefix=".fingerprint-template-",
        suffix=".tmp",
        delete=False,
    )
    temp_path = handle.name
    try:
        with handle:
            json.dump(payload, handle, separators=(",", ":"))
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_path, path)
    except BaseException:
        # Leave the previous template in place; only the temp file is discarded.
        try:
            os.unlink(temp_path)
        except OSError:
            pass
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, help="template JSON path")
    parser.add_argument(
        "--constant",
        default=DEFAULT_CONSTANT,
        help="constant.ts to read PIXEL_DEVICES from",
    )
    parser.add_argument("--workers", type=int, default=6)
    parser.add_argument(
        "--allow-partial",
        action="store_true",
        help="write devices that succeeded even if some products failed",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="only report problems",
    )
    args = parser.parse_args()

    try:
        devices = parse_devices(args.constant)
    except TemplateError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    try:
        template = build_template(devices, args.workers)
    except TemplateError as error:
        existing = load_existing(args.output)
        kept = f" (kept {len(existing)} existing devices)" if existing else " (no existing template)"
        print(f"error: fetch failed: {error}{kept}", file=sys.stderr)
        return 1
    except Exception as error:  # noqa: BLE001 - network stack is wide
        print(f"error: fetch failed: {error}", file=sys.stderr)
        return 1

    if not args.allow_partial:
        # A partial template would silently drop devices from the picker, so
        # only merge into the existing one when explicitly allowed.
        expected = {device["product"] for device in devices}
        if set(template) != expected:
            print(
                "error: refusing to write a partial template "
                f"({len(template)}/{len(expected)} devices)",
                file=sys.stderr,
            )
            return 1

    previous = load_existing(args.output) or {}
    merged: dict = {}
    for product, entry in template.items():
        old = previous.get(product)
        if old is None:
            merged[product] = entry
            continue
        # Never lose a build that a previous fetch knew about. Upstream can
        # retire old builds, and the picker should keep offering them.
        seen = {f"{row[0]}|{row[1]}" for row in entry["builds"]}
        extra = [
            row
            for row in old.get("builds", [])
            if f"{row[0]}|{row[1]}" not in seen
        ]
        merged[product] = {
            "model": entry["model"],
            "min": entry["min"],
            "max": entry["max"],
            "builds": entry["builds"] + extra,
        }

    total = sum(len(entry["builds"]) for entry in merged.values())
    atomic_write(args.output, merged)
    if not args.quiet:
        added = total - sum(len(entry.get("builds", [])) for entry in previous.values())
        size = os.path.getsize(args.output) / 1024
        print(
            f"fingerprint template: {len(merged)} devices, {total} builds "
            f"({added:+d}), {size:.1f} KiB -> {args.output}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
