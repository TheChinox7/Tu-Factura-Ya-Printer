#!/usr/bin/env python3
"""Rebuild and relaunch the native preview when its source changes."""

import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent
ASSETS = ROOT / "assets"
BINARY = "tu-factura-ya-printer"
APP_NAME = "Tu Factura Ya Printer"


def lock_watcher(file):
    if os.name == "nt":
        import msvcrt

        msvcrt.locking(file.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        import fcntl

        fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)


def snapshot():
    sources = {
        ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "build.rs",
        *ROOT.joinpath("src").rglob("*.rs"), *ASSETS.glob("app-icon*"),
    }
    revisions = {}
    for path in sources:
        try:
            stat = path.stat()
            revisions[path] = (stat.st_mtime_ns, stat.st_size)
        except FileNotFoundError:
            pass  # Editors can replace files while this scan is running.
    return revisions


def build(cargo, env):
    print("Compilando la vista de desarrollo…", flush=True)
    result = subprocess.run(
        [cargo, "build", "--locked", "--bin", BINARY, "--message-format=json"],
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        encoding="utf-8",
        errors="replace",
    )
    executable = None
    for line in result.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            print(line, flush=True)
            continue
        reason = event.get("reason") if isinstance(event, dict) else None
        if reason == "compiler-message" and event["message"].get("rendered"):
            print(event["message"]["rendered"], end="", file=sys.stderr, flush=True)
        if reason == "compiler-artifact" and event["target"]["name"] == BINARY:
            executable = event.get("executable") or executable
    if result.returncode or executable is None:
        print("La compilación falló. Se reintentará al detectar cambios.", flush=True)
        return None
    return executable


def stop_preview(preview):
    if preview is None or preview.poll() is not None:
        return
    preview.terminate()
    try:
        preview.wait(timeout=5)
    except subprocess.TimeoutExpired:
        preview.kill()
        preview.wait()


def mac_preview_binary():
    bundle = ROOT / "target" / f"{APP_NAME} — Desarrollo.app"
    return bundle / "Contents" / "MacOS" / BINARY


def mac_background_preview_pids():
    expected_command = f"{mac_preview_binary()} --dev --background"
    user = str(os.getuid())
    processes = subprocess.run(
        ["/bin/ps", "-axww", "-o", "uid=", "-o", "ppid=", "-o", "pid=", "-o", "command="],
        check=True,
        stdout=subprocess.PIPE,
        encoding="utf-8",
        errors="replace",
    )
    matches = []
    for line in processes.stdout.splitlines():
        fields = line.split(maxsplit=3)
        if len(fields) == 4:
            uid, parent, pid, command = fields
            if uid == user and parent == "1" and command == expected_command:
                matches.append(int(pid))
    return matches


def stop_mac_background_preview():
    running = mac_background_preview_pids()
    if not running:
        return True
    for pid in running:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass

    deadline = time.monotonic() + 3
    while mac_background_preview_pids():
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.1)
    return True


def preview_executable(executable):
    target = ROOT / "target"
    if sys.platform == "darwin":
        binary = mac_preview_binary()
        contents = binary.parent.parent
        binary.parent.mkdir(parents=True, exist_ok=True)
        resources = contents / "Resources"
        resources.mkdir(exist_ok=True)
        shutil.copy2(ASSETS / "app-icon.icns", resources)
        with (contents / "Info.plist").open("wb") as file:
            plistlib.dump({
                "CFBundleDisplayName": APP_NAME,
                "CFBundleExecutable": BINARY,
                "CFBundleIdentifier": "ec.tufacturaya.printer.dev",
                "CFBundleIconFile": "app-icon.icns",
                "CFBundleName": APP_NAME,
                "CFBundlePackageType": "APPL",
                "CFBundleVersion": "1",
                "LSEnvironment": {"TFY_PRINTER_DEV": "1"},
            }, file)
        # Replace the file so macOS sees a new executable and its new code signature together.
        staged = binary.with_suffix(".new")
        shutil.copy2(executable, staged)
        staged.replace(binary)
        subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", contents.parent], check=True)
        return binary
    if os.name == "nt":
        # Windows locks a running executable, so keep Cargo's output available.
        return shutil.copy2(executable, target / f"{BINARY}-dev.exe")
    return executable


def watch(cargo, env):
    previous = None
    preview = None
    try:
        while True:
            current = snapshot()
            if current != previous:
                previous = current
                executable = build(cargo, env)
                if executable is not None and snapshot() == current:
                    if (
                        sys.platform == "darwin"
                        and preview is None
                        and not stop_mac_background_preview()
                    ):
                        print(
                            "La vista en segundo plano sigue abierta. Se reintentará al detectar cambios.",
                            file=sys.stderr,
                            flush=True,
                        )
                        continue
                    stop_preview(preview)
                    preview = subprocess.Popen([preview_executable(executable), "--dev"], cwd=ROOT)
                    print("Vista de desarrollo iniciada. Esperando cambios…", flush=True)
            if preview is not None and (exit_code := preview.poll()) is not None:
                if exit_code != 0:
                    print(f"La vista se cerró con código {exit_code}.", flush=True)
                preview = None
            time.sleep(0.5)
    finally:
        stop_preview(preview)


def interrupted(_signal, _frame):
    raise KeyboardInterrupt


def main():
    cargo_name = "cargo.exe" if os.name == "nt" else "cargo"
    cargo = shutil.which(cargo_name) or Path.home() / ".cargo" / "bin" / cargo_name
    if not Path(cargo).is_file():
        print("No se encontró Cargo.", file=sys.stderr, flush=True)
        return 1
    env = os.environ.copy()
    env.setdefault("CARGO_PROFILE_DEV_DEBUG", "0")
    lock_path = ROOT / "target" / ".dev-watch.lock"
    lock_path.parent.mkdir(exist_ok=True)
    with lock_path.open("a+b") as file:
        file.seek(0)
        try:
            lock_watcher(file)
        except OSError:
            print("Ya hay un proceso de recarga abierto en esta carpeta.", file=sys.stderr, flush=True)
            return 1
        signal.signal(signal.SIGTERM, interrupted)
        try:
            watch(cargo, env)
        except KeyboardInterrupt:
            return 0


if __name__ == "__main__":
    sys.exit(main())
