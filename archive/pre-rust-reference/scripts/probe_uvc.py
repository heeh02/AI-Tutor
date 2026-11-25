"""Read-only DirectShow camera-control capability probe; never calls Set.

Enumerates native device monikers and reads IAMCameraControl GetRange/Get.
No filter graph, capture stream, pan/tilt movement, or vendor command is created.
"""
from __future__ import annotations

import argparse
import ctypes as C
import json
from pathlib import Path
import subprocess
import sys
import uuid


ROOT = Path(__file__).resolve().parents[1]
HRESULT = C.c_long
VOID = C.c_void_p
LONG = C.c_long
ULONG = C.c_ulong
GUID = C.c_ubyte * 16
PGUID = C.POINTER(GUID)
PVOID = C.POINTER(VOID)


def guid(value):
    return GUID.from_buffer_copy(uuid.UUID(value).bytes_le)


class _Value(C.Union):
    _fields_ = [("lVal", LONG), ("bstrVal", VOID), ("record", VOID * 2)]


class Variant(C.Structure):
    _anonymous_ = ("value",)
    _fields_ = [("vt", C.c_ushort), ("reserved1", C.c_ushort),
                ("reserved2", C.c_ushort), ("reserved3", C.c_ushort), ("value", _Value)]


def call(pointer, slot, argtypes, *args):
    table = C.cast(pointer, C.POINTER(C.POINTER(VOID))).contents
    method = C.WINFUNCTYPE(HRESULT, VOID, *argtypes)(table[slot])
    return method(pointer, *args)


def release(pointer):
    if pointer and pointer.value:
        call(pointer, 2, [])


def code(hr):
    return f"0x{hr & 0xffffffff:08X}"


def checked(hr, operation):
    if hr < 0:
        raise OSError(f"{operation}: {code(hr)}")


def property_read(bag, key, automation):
    value = Variant()
    automation.VariantInit(C.byref(value))
    try:
        hr = call(bag, 3, [C.c_wchar_p, C.POINTER(Variant), VOID], key, C.byref(value), None)
        checked(hr, f"IPropertyBag.Read({key})")
        return C.wstring_at(value.bstrVal) if value.vt == 8 and value.bstrVal else None
    finally:
        automation.VariantClear(C.byref(value))


def query_controls(moniker):
    filter_ptr, control = VOID(), VOID()
    try:
        # IMoniker BindToObject is slot8 after IPersistStream; only instantiate filter.
        iid_filter = guid("56A86895-0AD4-11CE-B03A-0020AF0BA770")
        hr = call(moniker, 8, [VOID, VOID, PGUID, PVOID], None, None, C.byref(iid_filter), C.byref(filter_ptr))
        checked(hr, "IMoniker.BindToObject(IBaseFilter)")
        iid_control = guid("C6E13370-30AC-11D0-A18C-00A0C9118956")
        hr = call(filter_ptr, 0, [PGUID, PVOID], C.byref(iid_control), C.byref(control))
        if hr < 0:
            return {"interface_supported": False, "hresult": code(hr), "properties": {}}
        properties = {}
        for name, number in [("pan", 0), ("tilt", 1)]:
            minimum, maximum, step, default, flags = [LONG() for _ in range(5)]
            # IAMCameraControl vtable: IUnknown0..2, GetRange3, Set4, Get5.
            # Slot4 is deliberately never referenced by this read-only probe.
            hr = call(control, 3, [LONG] + [C.POINTER(LONG)] * 5, number,
                      C.byref(minimum), C.byref(maximum), C.byref(step), C.byref(default), C.byref(flags))
            prop = {"get_range_hresult": code(hr), "range_supported": hr >= 0}
            if hr >= 0:
                prop.update(minimum=minimum.value, maximum=maximum.value, step=step.value,
                            default=default.value, capability_flags=flags.value,
                            manual_supported=bool(flags.value & 2), auto_supported=bool(flags.value & 1),
                            relative_flag=bool(flags.value & 16), unit_documented="degrees for absolute pan/tilt; not physically calibrated")
            current, current_flags = LONG(), LONG()
            hr = call(control, 5, [LONG, C.POINTER(LONG), C.POINTER(LONG)], number, C.byref(current), C.byref(current_flags))
            prop.update(get_current_hresult=code(hr))
            if hr >= 0:
                prop.update(current=current.value, current_flags=current_flags.value)
            properties[name] = prop
        return {"interface_supported": True, "properties": properties}
    finally:
        release(control)
        release(filter_ptr)


def probe(expected_path):
    ole = C.OleDLL("ole32")
    automation = C.OleDLL("oleaut32")
    ole.CoInitializeEx.argtypes, ole.CoInitializeEx.restype = [VOID, ULONG], HRESULT
    ole.CoCreateInstance.argtypes = [PGUID, VOID, ULONG, PGUID, PVOID]
    ole.CoCreateInstance.restype = HRESULT
    automation.VariantInit.argtypes = [C.POINTER(Variant)]
    automation.VariantClear.argtypes, automation.VariantClear.restype = [C.POINTER(Variant)], HRESULT
    checked(ole.CoInitializeEx(None, 0), "CoInitializeEx")
    enumerator, monikers = VOID(), VOID()
    report = {"date": "2026-11-01", "read_only": True, "set_calls": 0,
              "capture_graph_created": False, "devices": []}
    try:
        clsid = guid("62BE5D10-60EB-11D0-BD3B-00A0C911CE86")
        iid = guid("29840822-5B84-11D0-BD3B-00A0C911CE86")
        checked(ole.CoCreateInstance(C.byref(clsid), None, 1, C.byref(iid), C.byref(enumerator)), "CoCreateInstance(SystemDeviceEnum)")
        category = guid("860BB310-5D01-11D0-BD3B-00A0C911CE86")
        hr = call(enumerator, 3, [PGUID, PVOID, ULONG], C.byref(category), C.byref(monikers), 0)
        checked(hr, "CreateClassEnumerator(VideoInputDeviceCategory)")
        if hr == 1:
            return {**report, "result": "NO_DEVICES"}
        while True:
            moniker, fetched, bag = VOID(), ULONG(), VOID()
            hr = call(monikers, 3, [ULONG, PVOID, C.POINTER(ULONG)], 1, C.byref(moniker), C.byref(fetched))
            if hr != 0:
                break
            try:
                iid_bag = guid("55272A00-42CB-11CE-8135-00AA004BB851")
                checked(call(moniker, 9, [VOID, VOID, PGUID, PVOID], None, None, C.byref(iid_bag), C.byref(bag)), "IMoniker.BindToStorage")
                name = property_read(bag, "FriendlyName", automation)
                path = property_read(bag, "DevicePath", automation)
                matches = bool(path and path.casefold() == expected_path.casefold())
                device = {"name": name, "device_path": path, "matches_saved_stable_path": matches}
                if matches:
                    try:
                        device["camera_control"] = query_controls(moniker)
                    except Exception as exc:
                        device["camera_control"] = {"error": str(exc)}
                report["devices"].append(device)
            finally:
                release(bag)
                release(moniker)
        report["result"] = "PASS_READ_ONLY_QUERY" if any(d["matches_saved_stable_path"] for d in report["devices"]) else "DEVICE_NOT_MATCHED"
        return report
    finally:
        release(monikers)
        release(enumerator)
        ole.CoUninitialize()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--out", default="runtime/uvc_probe.json")
    args = parser.parse_args()
    if args.child:
        saved = ROOT / "runtime" / "camera_device.txt"
        if not saved.exists():
            raise RuntimeError("First discover/select the camera using the application")
        print(json.dumps(probe(saved.read_text(encoding="utf8")), ensure_ascii=True))
    else:
        # A wedged native driver cannot block the calling workflow indefinitely.
        completed = subprocess.run([sys.executable, str(Path(__file__).resolve()), "--child"],
                                   capture_output=True, text=True, timeout=20,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        if completed.returncode:
            raise RuntimeError(completed.stderr[-2000:])
        report = json.loads(completed.stdout)
        output = ROOT / args.out
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf8")
        print(json.dumps(report, ensure_ascii=True, indent=2))


if __name__ == "__main__":
    main()
