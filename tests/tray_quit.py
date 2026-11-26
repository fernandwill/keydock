"""Run against a keydock binary on an interactive Windows desktop."""

import ctypes as c
from ctypes import wintypes as w
from pathlib import Path
import subprocess
import sys
import time

user32 = c.WinDLL("user32", use_last_error=True)
TIMEOUT = 5
# tray-icon 0.26 uses this private window class and shell callback message.
TRAY_CLASS = "tray_icon_app"
TRAY_CALLBACK = 6002
WM_RBUTTONUP = 0x0205
WM_KEYDOWN = 0x0100
MN_GETHMENU = 0x01E1
MF_BYPOSITION = 0x0400
VK_DOWN, VK_RETURN = 0x28, 0x0D


def window(process, class_name):
    deadline = time.monotonic() + TIMEOUT
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"keydock exited before {class_name} appeared")
        hwnd = 0
        while hwnd := user32.FindWindowExW(0, hwnd, class_name, None):
            owner = w.DWORD()
            user32.GetWindowThreadProcessId(hwnd, c.byref(owner))
            if owner.value == process.pid:
                return hwnd
        time.sleep(0.02)
    raise TimeoutError(f"No {class_name} window for keydock PID {process.pid}")


def post(hwnd, message, key, data):
    if not user32.PostMessageW(hwnd, message, key, data):
        raise c.WinError(c.get_last_error())


def check_second_instance_exits(binary):
    second = subprocess.Popen([binary])
    try:
        if second.wait(timeout=TIMEOUT) != 0:
            raise RuntimeError(f"Second keydock returned exit code {second.returncode}")
    except subprocess.TimeoutExpired:
        second.kill()
        second.wait(timeout=TIMEOUT)
        raise RuntimeError("A second keydock kept running next to the first") from None


def check_quit(binary):
    process = subprocess.Popen([binary])
    try:
        tray = window(process, TRAY_CLASS)
        check_second_instance_exits(binary)
        post(tray, TRAY_CALLBACK, 0, WM_RBUTTONUP)
        popup = window(process, "#32768")
        menu = c.c_size_t()
        if not user32.SendMessageTimeoutW(popup, MN_GETHMENU, 0, 0, 0, TIMEOUT * 1000, c.byref(menu)):
            raise RuntimeError("Popup did not return its menu handle")
        label = c.create_unicode_buffer(128)
        user32.GetMenuStringW(w.HMENU(menu.value), 0, label, len(label), MF_BYPOSITION)
        if label.value != "Quit":
            raise RuntimeError(f"Expected Quit as the first item, found {label.value!r}")
        post(popup, WM_KEYDOWN, VK_DOWN, 0)
        post(popup, WM_KEYDOWN, VK_RETURN, 0)
        if process.wait(timeout=TIMEOUT) != 0:
            raise RuntimeError(f"Quit returned exit code {process.returncode}")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=TIMEOUT)


if __name__ == "__main__":
    check_quit(Path(sys.argv[1]).resolve())
    print("PASS: a second keydock exited, the native menu opened, and Quit exited with status 0.")
