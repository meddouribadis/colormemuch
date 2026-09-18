from pathlib import Path
import sys
import time

import frida

OPENRGB = r"C:\WINDOWS\System32\DriverStore\FileRepository\predatorservice.inf_amd64_c95114a8e07b0c8a\OpenRGB.exe"
SCRIPT = Path(__file__).with_name("frida_wmi_trace.js")


def on_message(message, data):
    if message["type"] == "send":
        print(message["payload"], flush=True)
    elif message["type"] == "error":
        print(message["stack"], file=sys.stderr, flush=True)
    else:
        print(message, flush=True)


def main():
    device = frida.get_local_device()
    existing = [
        process for process in device.enumerate_processes()
        if process.name and process.name.lower() == "openrgb.exe"
    ]
    if existing:
        pids = ", ".join(str(process.pid) for process in existing)
        raise SystemExit(
            f"OpenRGB is already running (PID {pids}). "
            "Close it and rerun this launcher."
        )

    pid = device.spawn([OPENRGB, "--server"])
    session = device.attach(pid)
    script = session.create_script(SCRIPT.read_text(encoding="utf-8"))
    script.on("message", on_message)
    script.load()
    device.resume(pid)

    print(f"OpenRGB spawned under Frida, PID={pid}", flush=True)
    print("Leave this window open, then use PredatorSense. Ctrl+C stops the trace.", flush=True)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        session.detach()
        try:
            device.kill(pid)
        except frida.ProcessNotFoundError:
            pass


if __name__ == "__main__":
    main()
