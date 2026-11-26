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
shell32 = c.WinDLL("shell32")
kernel32.CreateMutexW.restype = w.HANDLE
shell32.Shell_NotifyIconGetRect.restype = c.c_long
TIMEOUT = 5
# tray-icon 0.26 uses this private window class, shell callback message and icon id
# (one counter gives the public id 1 and the internal id 2).
TRAY_CLASS = "tray_icon_app"
TRAY_CALLBACK = 6002
TRAY_ICON_ID = 2
WM_NULL = 0x0000
WM_RBUTTONUP = 0x0205
WM_CANCELMODE = 0x001F
WM_COMMAND = 0x0111
MN_GETHMENU = 0x01E1
MF_BYPOSITION = 0x0400
SMTO_ABORTIFHUNG = 0x0002
MUTEX_NAME = r"Local\keydock"
ERROR_ALREADY_EXISTS = 183
SDDL_REVISION_1 = 1


class SECURITY_ATTRIBUTES(c.Structure):
    _fields_ = [("nLength", w.DWORD), ("lpSecurityDescriptor", w.LPVOID), ("bInheritHandle", w.BOOL)]


class NOTIFYICONIDENTIFIER(c.Structure):
    _fields_ = [("cbSize", w.DWORD), ("hWnd", w.HWND), ("uID", w.UINT), ("guidItem", c.c_ubyte * 16)]


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


def menu_diagnosis(tray):
    # tray-icon skips the menu without an error when GetCursorPos or Shell_NotifyIconGetRect fails.
    icon = NOTIFYICONIDENTIFIER(c.sizeof(NOTIFYICONIDENTIFIER), tray, TRAY_ICON_ID)
    rect_result = shell32.Shell_NotifyIconGetRect(c.byref(icon), c.byref(w.RECT())) & 0xFFFFFFFF
    cursor_ok = bool(user32.GetCursorPos(c.byref(w.POINT())))
    reply = c.c_size_t()
    answers = bool(user32.SendMessageTimeoutW(tray, WM_NULL, 0, 0, SMTO_ABORTIFHUNG, TIMEOUT * 1000, c.byref(reply)))
    foreground = c.create_unicode_buffer(64)
    user32.GetClassNameW(user32.GetForegroundWindow(), foreground, len(foreground))
    return (
        f"Shell_NotifyIconGetRect={rect_result:#010x}, GetCursorPos ok={cursor_ok},"
        f" keydock answers messages={answers}, foreground window class={foreground.value!r}"
    )


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
        try:
            popup = window(process, "#32768")
        except TimeoutError:
            raise RuntimeError(f"The tray menu did not open. {menu_diagnosis(tray)}") from None
        menu = c.c_size_t()
        if not user32.SendMessageTimeoutW(popup, MN_GETHMENU, 0, 0, 0, TIMEOUT * 1000, c.byref(menu)):
            raise RuntimeError("Popup did not return its menu handle")
        hmenu = w.HMENU(menu.value)
        label = c.create_unicode_buffer(128)
        labels = []
        for position in range(user32.GetMenuItemCount(hmenu)):
            user32.GetMenuStringW(hmenu, position, label, len(label), MF_BYPOSITION)
            labels.append(label.value)
        if "Quit" not in labels:
            raise RuntimeError(f"No Quit item in the menu, found {labels!r}")
        # Replay a click: close the menu, then send its command. Posted keystrokes are unreliable
        # because the menu can drop them when keydock is not foreground, and a command that arrives
        # while the menu is open loses the quit inside the menu loop.
        quit_id = user32.GetMenuItemID(hmenu, labels.index("Quit"))
        if quit_id == -1:
            raise RuntimeError("Quit has no command id")
        post(tray, WM_CANCELMODE, 0, 0)
        post(tray, WM_COMMAND, quit_id, 0)
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
