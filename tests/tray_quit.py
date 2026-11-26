"""Run against a keydock binary on an interactive Windows desktop."""

import ctypes as c
from ctypes import wintypes as w
from pathlib import Path
import subprocess
import sys
import time

user32 = c.WinDLL("user32", use_last_error=True)
kernel32 = c.WinDLL("kernel32", use_last_error=True)
advapi32 = c.WinDLL("advapi32", use_last_error=True)
kernel32.CreateMutexW.restype = w.HANDLE
TIMEOUT = 5
# tray-icon 0.26 uses this private window class and shell callback message.
TRAY_CLASS = "tray_icon_app"
TRAY_CALLBACK = 6002
WM_RBUTTONUP = 0x0205
WM_KEYDOWN = 0x0100
MN_GETHMENU = 0x01E1
MF_BYPOSITION = 0x0400
VK_DOWN, VK_RETURN = 0x28, 0x0D
MUTEX_NAME = r"Local\keydock"
ERROR_ALREADY_EXISTS = 183
SDDL_REVISION_1 = 1


class SECURITY_ATTRIBUTES(c.Structure):
    _fields_ = [("nLength", w.DWORD), ("lpSecurityDescriptor", w.LPVOID), ("bInheritHandle", w.BOOL)]


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


def check_exits_quietly(binary, situation):
    process = subprocess.Popen([binary])
    try:
        if process.wait(timeout=TIMEOUT) != 0:
            raise RuntimeError(f"keydock returned exit code {process.returncode} {situation}")
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=TIMEOUT)
        raise RuntimeError(f"keydock kept running {situation}") from None


def check_unopenable_lock_exits(binary):
    # An empty DACL stands in for an elevated keydock's lock, which a normal process cannot open.
    descriptor = w.LPVOID()
    if not advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW(
        "D:", SDDL_REVISION_1, c.byref(descriptor), None
    ):
        raise c.WinError(c.get_last_error())
    attributes = SECURITY_ATTRIBUTES(c.sizeof(SECURITY_ATTRIBUTES), descriptor, False)
    c.set_last_error(0)
    mutex = kernel32.CreateMutexW(c.byref(attributes), False, MUTEX_NAME)
    error = c.get_last_error()
    kernel32.LocalFree(descriptor)
    if not mutex:
        raise c.WinError(error)
    try:
        if error == ERROR_ALREADY_EXISTS:
            raise RuntimeError("Another keydock is running; quit it before this check")
        check_exits_quietly(binary, "when it cannot open another instance's lock")
    finally:
        kernel32.CloseHandle(mutex)


def check_quit(binary):
    process = subprocess.Popen([binary])
    try:
        tray = window(process, TRAY_CLASS)
        check_exits_quietly(binary, "next to the first")
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
    binary = Path(sys.argv[1]).resolve()
    check_unopenable_lock_exits(binary)
    check_quit(binary)
    print(
        "PASS: keydock exited quietly when the lock was taken or could not be opened,"
        " the native menu opened, and Quit exited with status 0."
    )
