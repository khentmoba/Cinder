#!/usr/bin/env python3
"""
Cinder Discord Release Tool
Uploads the Windows installer and announces the release via Webhook.
"""

import os
import sys
import json
import argparse
import mimetypes
import re
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit
from urllib.request import Request, urlopen

PROJECT_ROOT = Path(__file__).resolve().parent.parent
REPO = "khentmoba/Cinder"
# The upload budget Discord allows for a free webhook attachment.
MAX_UPLOAD_BYTES = 10 * 1024 * 1024
# A leading bullet marker, with the space that usually follows it.
BULLET_RE = re.compile(r"^[-*]\s+")


def get_webhook_url(explicit_url=None):
    if explicit_url:
        return explicit_url.strip()

    env_url = os.environ.get("DISCORD_WEBHOOK_URL", "").strip()
    if env_url:
        return env_url

    candidates = [
        PROJECT_ROOT / "scripts" / "discord_webhook.txt",
        PROJECT_ROOT / "discord_webhook.txt",
    ]
    for env_key in ("LOCALAPPDATA", "APPDATA"):
        base = os.environ.get(env_key)
        if base:
            candidates.append(Path(base) / "Cinder" / "discord_webhook.txt")

    for c in candidates:
        if c.is_file():
            url = c.read_text(encoding="utf-8").strip()
            if url.startswith("http"):
                return url
    return None


def get_version():
    manifest = PROJECT_ROOT / "package.json"
    if manifest.is_file():
        data = json.loads(manifest.read_text(encoding="utf-8-sig"))
        version = data.get("version")
        if version:
            return version
    return "0.0.0"


def send_webhook(webhook_url, payload, file_path=None):
    parts = urlsplit(webhook_url)
    query = dict(parse_qsl(parts.query))
    query["wait"] = "true"
    webhook_url = urlunsplit(parts._replace(query=urlencode(query)))
    if not file_path:
        req = Request(
            webhook_url,
            data=json.dumps(payload).encode("utf-8"),
            headers={"Content-Type": "application/json", "User-Agent": "CinderReleaseBot/1.0"},
        )
        with urlopen(req) as resp:
            return json.load(resp)

    boundary = "----WebKitFormBoundaryCinderRelease7MA4YWxkTrZu0gW"
    file_bytes = file_path.read_bytes()
    filename = file_path.name
    mime_type = mimetypes.guess_type(filename)[0] or "application/octet-stream"

    body = [
        f"--{boundary}".encode("utf-8"),
        b'Content-Disposition: form-data; name="payload_json"',
        b"Content-Type: application/json\r\n",
        json.dumps(payload).encode("utf-8"),
        b"",
        f"--{boundary}".encode("utf-8"),
        f'Content-Disposition: form-data; name="files[0]"; filename="{filename}"'.encode("utf-8"),
        f"Content-Type: {mime_type}\r\n".encode("utf-8"),
        file_bytes,
        f"--{boundary}--\r\n".encode("utf-8"),
    ]

    data = b"\r\n".join(body)
    req = Request(
        webhook_url,
        data=data,
        headers={
            "Content-Type": f"multipart/form-data; boundary={boundary}",
            "User-Agent": "CinderReleaseBot/1.0",
            "Content-Length": str(len(data)),
        },
    )
    with urlopen(req) as resp:
        return json.load(resp)


def main():
    parser = argparse.ArgumentParser(description="Send a Cinder release notification to Discord.")
    parser.add_argument("--version", "-v", help="Release version (defaults to package.json)")
    parser.add_argument("--summary", "-s", help="Release notes, one per line")
    parser.add_argument("--exe", help="Path to the Windows installer")
    parser.add_argument("--no-upload", action="store_true", help="Link the release instead of uploading it")
    parser.add_argument("--webhook", help="Explicit webhook URL")
    parser.add_argument("--dry-run", action="store_true", help="Print the payload without sending it")
    args = parser.parse_args()

    webhook_url = get_webhook_url(args.webhook)
    if not webhook_url and not args.dry_run:
        print("[Cinder] Error: no Discord webhook URL found.")
        print("  Set DISCORD_WEBHOOK_URL, or place the URL in %LOCALAPPDATA%\\Cinder\\discord_webhook.txt")
        sys.exit(1)

    ver = args.version or get_version()
    notes = args.summary
    if notes is None:
        notes_path = PROJECT_ROOT / "release-notes.md"
        notes = notes_path.read_text(encoding="utf-8-sig") if notes_path.is_file() else ""
    # Only the bullet marker comes off, so a bold label keeps its asterisks.
    bullets = "\n".join(
        "- " + BULLET_RE.sub("", line).strip() for line in notes.splitlines() if line.strip()
    )
    if not bullets:
        parser.error("Release notes are required")
    if len(bullets) > 1024:
        parser.error("Release notes must fit the 1024-character 'What's New' field")

    installer = PROJECT_ROOT / "src-tauri" / "target" / "release" / "bundle" / "nsis" / f"Cinder_{ver}_x64-setup.exe"
    exe_file = None
    if not args.no_upload:
        candidate = Path(args.exe) if args.exe else installer
        # A bigger installer uses the public release asset instead of the upload.
        if candidate.is_file() and candidate.stat().st_size <= MAX_UPLOAD_BYTES:
            exe_file = candidate

    download_url = f"https://github.com/{REPO}/releases/download/v{ver}/{installer.name}"
    delivery = (
        {
            "name": "Attached File",
            "value": f"{exe_file.name} ({exe_file.stat().st_size / (1024 * 1024):.2f} MB)",
            "inline": True,
        }
        if exe_file
        else {"name": "Download", "value": f"[{installer.name}]({download_url})", "inline": True}
    )

    payload = {
        "username": "Cinder Release Bot",
        "content": "New Release: **Cinder v" + ver + "** is now available! "
        + ("Download the attached installer." if exe_file else "Download the installer using the link below."),
        "allowed_mentions": {"parse": []},
        "embeds": [
            {
                "title": f"Release: Cinder v{ver}",
                "description": "Run the installer to install or update Cinder on Windows.",
                "color": 4363994,
                "fields": [
                    {"name": "Version", "value": f"v{ver}", "inline": True},
                    {"name": "Configuration", "value": "Windows x64 / NSIS", "inline": True},
                    delivery,
                    {"name": "What's New", "value": bullets, "inline": False},
                ],
                "footer": {"text": "Cinder Automated Release System"},
                "timestamp": datetime.now(timezone.utc).isoformat(),
            }
        ],
    }

    if args.dry_run:
        print("[DryRun] Release payload:", json.dumps(payload, indent=2))
        if exe_file:
            print(f"[DryRun] Attaching: {exe_file}")
        else:
            print(f"[DryRun] Linking: {download_url}")
        return

    print(f"[Cinder] Publishing v{ver} to Discord...")
    if exe_file:
        print(f"[Cinder] Attaching: {exe_file.name}")
    message = send_webhook(webhook_url, payload, file_path=exe_file)
    print(f"[Cinder] Discord message ID: {message['id']}")
    print(f"[Cinder] Published v{ver} to #cinder-releases.")


if __name__ == "__main__":
    main()
