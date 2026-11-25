"""Bounded Windows native absolute PTZ, manual small steps only.

No vendor protocol, continuous velocity, or automatic tracking is implemented.
Each native transaction has one child process; constructors never move hardware.
"""
from __future__ import annotations

import ctypes as C
import json
from pathlib import Path
import subprocess
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
_CONTROLLER_LOCK = threading.Lock()
_AXES = {"pan": 0, "tilt": 1}


def calibration_result(root):
    try:
        report=json.loads((Path(root)/"runtime"/"ptz_motion_probe.json").read_text(encoding="utf8"))
        return report.get("result","NOT_RUN")
    except (OSError,ValueError):
        return "NOT_RUN"


def validate_step(axis, delta, prop, *, target=None):
    if axis not in _AXES:
        raise ValueError("Only pan/tilt are allowed")
    if isinstance(delta, bool) or not isinstance(delta, int) or abs(delta) > 2 or delta == 0:
        raise ValueError("Manual motion is limited to integer steps of 1 or 2 degrees")
    if not prop.get("manual_supported") or prop.get("relative_flag"):
        raise ValueError("Verified absolute manual control is required")
    if prop.get("step", 0) <= 0 or "current" not in prop:
        raise ValueError("Current position and positive driver step are required")
    value = prop["current"] + delta if target is None else target
    if not isinstance(value, int) or isinstance(value, bool) or abs(value-prop["current"]) > 2:
        raise ValueError("Absolute target must stay within two degrees of fresh readback")
    if not prop["minimum"] <= value <= prop["maximum"]:
        raise ValueError("Target is outside the advertised camera range")
    if (value - prop["minimum"]) % prop["step"]:
        raise ValueError("Target is not aligned to driver step")
    return value


_LONG, _VOID = C.c_long, C.c_void_p
_GUID = C.c_ubyte * 16
_PGUID, _PVOID = C.POINTER(_GUID), C.POINTER(_VOID)


def _guid(value):
    return _GUID.from_buffer_copy(uuid.UUID(value).bytes_le)


class _VariantValue(C.Union):
    _fields_ = [("lVal", _LONG), ("bstrVal", _VOID), ("record", _VOID * 2)]


class _Variant(C.Structure):
    _anonymous_ = ("value",)
    _fields_ = [("vt", C.c_ushort), ("reserved1", C.c_ushort),
                ("reserved2", C.c_ushort), ("reserved3", C.c_ushort), ("value", _VariantValue)]


def _call(pointer, slot, argtypes, *args):
    table = C.cast(pointer, C.POINTER(C.POINTER(_VOID))).contents
    return C.WINFUNCTYPE(_LONG, _VOID, *argtypes)(table[slot])(pointer, *args)


def _release(pointer):
    if pointer and pointer.value:
        _call(pointer, 2, [])


def _check(hr, operation):
    if hr < 0:
        raise OSError(f"{operation}: 0x{hr & 0xffffffff:08X}")


class _CameraControl:
    """One COM apartment and filter reference; no capture graph/stream."""
    def __init__(self, device_path):
        self.device_path = device_path
        self.control, self.filter = _VOID(), _VOID()
        self.initialized = False

    def __enter__(self):
        self.ole = C.OleDLL("ole32")
        automation = C.OleDLL("oleaut32")
        self.ole.CoInitializeEx.argtypes, self.ole.CoInitializeEx.restype = [_VOID, C.c_ulong], _LONG
        self.ole.CoCreateInstance.argtypes = [_PGUID, _VOID, C.c_ulong, _PGUID, _PVOID]
        self.ole.CoCreateInstance.restype = _LONG
        automation.VariantInit.argtypes = [C.POINTER(_Variant)]
        automation.VariantClear.argtypes = [C.POINTER(_Variant)]
        _check(self.ole.CoInitializeEx(None, 0), "CoInitializeEx")
        self.initialized = True
        enumerator, monikers = _VOID(), _VOID()
        entered=False
        try:
            clsid = _guid("62BE5D10-60EB-11D0-BD3B-00A0C911CE86")
            iid_enum = _guid("29840822-5B84-11D0-BD3B-00A0C911CE86")
            _check(self.ole.CoCreateInstance(C.byref(clsid),None,1,C.byref(iid_enum),C.byref(enumerator)), "CreateSystemDeviceEnum")
            category = _guid("860BB310-5D01-11D0-BD3B-00A0C911CE86")
            hr = _call(enumerator,3,[_PGUID,_PVOID,C.c_ulong],C.byref(category),C.byref(monikers),0)
            _check(hr, "EnumerateVideoDevices")
            if hr == 1:
                raise ValueError("No camera is connected")
            while True:
                moniker, bag, fetched = _VOID(), _VOID(), C.c_ulong()
                if _call(monikers,3,[C.c_ulong,_PVOID,C.POINTER(C.c_ulong)],1,C.byref(moniker),C.byref(fetched)) != 0:
                    break
                try:
                    iid_bag = _guid("55272A00-42CB-11CE-8135-00AA004BB851")
                    _check(_call(moniker,9,[_VOID,_VOID,_PGUID,_PVOID],None,None,C.byref(iid_bag),C.byref(bag)), "BindPropertyBag")
                    value = _Variant()
                    automation.VariantInit(C.byref(value))
                    try:
                        _check(_call(bag,3,[C.c_wchar_p,C.POINTER(_Variant),_VOID],"DevicePath",C.byref(value),None), "ReadDevicePath")
                        path = C.wstring_at(value.bstrVal) if value.vt == 8 and value.bstrVal else None
                    finally:
                        automation.VariantClear(C.byref(value))
                    if path is None or path.casefold() != self.device_path.casefold():
                        continue
                    iid_filter = _guid("56A86895-0AD4-11CE-B03A-0020AF0BA770")
                    _check(_call(moniker,8,[_VOID,_VOID,_PGUID,_PVOID],None,None,C.byref(iid_filter),C.byref(self.filter)), "BindCameraFilter")
                    iid_control = _guid("C6E13370-30AC-11D0-A18C-00A0C9118956")
                    _check(_call(self.filter,0,[_PGUID,_PVOID],C.byref(iid_control),C.byref(self.control)), "QueryIAMCameraControl")
                    entered=True
                    return self
                finally:
                    _release(bag)
                    _release(moniker)
            raise ValueError("Saved camera device path is disconnected or changed")
        finally:
            _release(monikers)
            _release(enumerator)
            if not entered:
                self.__exit__(None,None,None)

    def read(self):
        result = {}
        for name, number in _AXES.items():
            minimum,maximum,step,default,flags = [_LONG() for _ in range(5)]
            _check(_call(self.control,3,[_LONG]+[C.POINTER(_LONG)]*5,number,
                    C.byref(minimum),C.byref(maximum),C.byref(step),C.byref(default),C.byref(flags)), f"GetRange({name})")
            current,current_flags = _LONG(),_LONG()
            _check(_call(self.control,5,[_LONG,C.POINTER(_LONG),C.POINTER(_LONG)],number,C.byref(current),C.byref(current_flags)),f"Get({name})")
            result[name] = {"minimum":minimum.value,"maximum":maximum.value,"step":step.value,
                "default":default.value,"current":current.value,"capability_flags":flags.value,
                "current_flags":current_flags.value,"manual_supported":bool(flags.value&2),
                "auto_supported":bool(flags.value&1),"relative_flag":bool(flags.value&16)}
        return result

    def set_absolute(self, axis, value):
        _check(_call(self.control,4,[_LONG,_LONG,_LONG],_AXES[axis],value,2),f"Set({axis})")

    def __exit__(self,*args):
        _release(self.control)
        _release(self.filter)
        self.control,self.filter = _VOID(),_VOID()
        if self.initialized:
            self.ole.CoUninitialize()
            self.initialized = False


def _transaction(request):
    path = request.get("device_path")
    if not isinstance(path,str) or not path:
        raise ValueError("An exact native device path is required")
    action = request.get("action")
    if action not in {"read","move"}:
        raise ValueError("Only read or bounded manual move is allowed")
    if action=="move" and calibration_result(ROOT)=="FAIL":
        raise ValueError("Physical commands frozen after failed calibration: uncontrolled tilt drift; read-only access remains available")
    with _CameraControl(path) as camera:
        before = camera.read()
        if action == "read":
            return {"properties":before,"unit":"degrees","interface":"IAMCameraControl","device_path":path}
        axis,delta = request.get("axis"),request.get("delta")
        prop = before.get(axis,{})
        target = validate_step(axis,delta,prop,target=request.get("target"))
        camera.set_absolute(axis,target)
        # An absolute target is finite. No speed command or autonomous repetition is used.
        time.sleep(.15)
        after = camera.read()
        return {"axis":axis,"delta":target-prop["current"],"before":before,"target":target,
                "after":after,"set_hresult":"S_OK","readback_matches":after[axis]["current"]==target,
                "other_axis_unchanged":all(after[a]["current"]==before[a]["current"] for a in _AXES if a != axis),
                "physical_visual_verification":"NOT_RUN"}


class NativePTZ:
    def __init__(self,root=ROOT,device_path=None,*,executor=None):
        self.root = Path(root)
        self.device_path = device_path
        self._executor = executor
        self._child = None
        self._state_lock = threading.RLock()
        self._generation = 0
        self._manual_enabled = False
        self._capabilities = None
        self._last_motion = None
        self._error = None
        self._last_move_at = 0
        # Cached evidence only: construction/status do not initialize COM or move hardware.
        try:
            path=self._path()
            report=json.loads((self.root/"runtime"/"uvc_probe.json").read_text(encoding="utf8"))
            device=next(d for d in report["devices"] if d.get("device_path","").casefold()==path.casefold())
            controls=device["camera_control"]
            if controls.get("interface_supported"):
                self._capabilities={"properties":controls["properties"],"device_path":path,
                                    "unit":"degrees","interface":"IAMCameraControl","cached_from_read_only_probe":True}
        except (OSError,ValueError,KeyError,StopIteration):
            pass

    def _path(self):
        if self.device_path:
            return self.device_path
        return (self.root/"runtime"/"camera_device.txt").read_text(encoding="utf8")

    def _run(self,action,*,axis=None,delta=None,target=None,generation=None):
        request = {"action":action,"device_path":self._path()}
        if action == "move":
            request.update(axis=axis,delta=delta)
            if target is not None:
                request["target"] = target
        if self._executor:
            return self._executor(request)
        with self._state_lock:
            if generation is not None and generation != self._generation:
                raise ValueError("Manual operation was stopped")
            child = subprocess.Popen([sys.executable,str(Path(__file__).resolve()),"--native-worker"],
                stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                creationflags=subprocess.CREATE_NO_WINDOW)
            self._child = child
        try:
            out,err = child.communicate(json.dumps(request).encode("utf8"),timeout=5)
            try:
                result = json.loads(out)
            except (ValueError,UnicodeError):
                raise ValueError(f"Native PTZ operation failed: {err.decode('utf8',errors='replace')[-500:]}")
            if not result.get("ok"):
                raise ValueError(result.get("error","Native PTZ failed"))
            if child.returncode:
                raise ValueError("Native PTZ worker exited unexpectedly")
            return result["result"]
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate(timeout=2)
            raise ValueError("Native camera-control operation timed out; no repeated commands sent")
        finally:
            with self._state_lock:
                if self._child is child:
                    self._child = None

    def probe(self):
        if not _CONTROLLER_LOCK.acquire(timeout=.5):
            raise ValueError("Another native camera-control operation is active")
        try:
            self._capabilities = self._run("read")
            self._error = None
        except Exception as exc:
            self._error = str(exc)
            raise
        finally:
            _CONTROLLER_LOCK.release()
        return self.status()

    def status(self):
        calibration=calibration_result(self.root)
        return {"mode":"manual" if self._manual_enabled else "off",
                "status":"NATIVE_ABSOLUTE_AVAILABLE" if self._capabilities else "NOT_PROBED",
                "hardware_read_capable":self._capabilities is not None,
                "low_amplitude_calibration":calibration,
                "external_follow":"BLOCKED",
                "manual_driver_verified":calibration=="PASS_DEVICE_READBACK",
                "manual_enabled":self._manual_enabled,"capabilities":self._capabilities,
                "vendor_auto_state":"unknown","external_yolo_enabled":False,
                "physical_follow_verified":False,"last_motion":self._last_motion,"error":self._error,
                "max_step_degrees":2,"continuous_speed":False,
                "reason":"Low-amplitude calibration failed: pan was restored, but tilt drifted without a tilt command; all physical commands frozen pending verified vendor tracking-off state and calibration" if calibration=="FAIL" else "Vendor tracking state is unverified; automatic following disabled",
                "stop_guarantee":"Stops queued/pending application transactions; no independently verified device emergency-stop command. Already accepted finite absolute target may complete."}

    def enable_manual(self,*,for_calibration=False):
        calibration=calibration_result(self.root)
        if calibration=="FAIL":
            raise ValueError("All physical commands are frozen after failed calibration and uncontrolled tilt drift")
        if calibration!="PASS_DEVICE_READBACK" and not for_calibration:
            raise ValueError("A successful low-amplitude calibration is required before manual mode")
        if not self._capabilities:
            self.probe()
        for prop in self._capabilities["properties"].values():
            if not prop["manual_supported"] or prop["relative_flag"]:
                raise ValueError("Absolute manual support is required")
        with self._state_lock:
            self._generation += 1
            self._manual_enabled = True
        return self.status()

    def manual_move(self,axis,delta,*,target=None):
        if calibration_result(self.root)=="FAIL":
            raise ValueError("All physical commands are frozen after failed calibration and uncontrolled tilt drift")
        # Validate user-facing fields before spawning anything.
        if axis not in _AXES or isinstance(delta,bool) or not isinstance(delta,int) or delta==0 or abs(delta)>2:
            raise ValueError("Only integer pan/tilt steps of at most two degrees are allowed")
        with self._state_lock:
            if not self._manual_enabled:
                raise ValueError("Explicitly enable manual mode before moving")
            if time.monotonic()-self._last_move_at < .5:
                raise ValueError("Manual steps are limited to one transaction per half second")
            generation = self._generation
        if not _CONTROLLER_LOCK.acquire(blocking=False):
            raise ValueError("Another native camera-control operation is active")
        try:
            with self._state_lock:
                if generation != self._generation or not self._manual_enabled:
                    raise ValueError("Manual operation was stopped")
            result = self._run("move",axis=axis,delta=delta,target=target,generation=generation)
            with self._state_lock:
                if generation != self._generation:
                    raise ValueError("Manual result discarded after stop")
                self._last_motion = result
                self._last_move_at = time.monotonic()
            return result
        except Exception as exc:
            self._error = str(exc)
            raise
        finally:
            _CONTROLLER_LOCK.release()

    def set_mode(self,mode):
        if mode == "off":
            return self.stop()
        if mode == "manual":
            return self.enable_manual()
        raise ValueError("Automatic/vendor tracking state is unverified; automatic modes remain disabled")

    def stop(self):
        with self._state_lock:
            self._generation += 1
            self._manual_enabled = False
            child = self._child
            if child and child.poll() is None:
                child.kill()
        return {"mode":"off","application_transactions_cancelled":True,
                "hardware_stop_command_sent":False,"continuous_speed_was_used":False,
                "device_emergency_stop_verified":False}

    def close(self):
        return self.stop()


if __name__ == "__main__":
    if sys.argv[1:] != ["--native-worker"]:
        raise SystemExit("This module exposes only the internal bounded worker")
    try:
        request=json.loads(sys.stdin.buffer.read(5000))
        print(json.dumps({"ok":True,"result":_transaction(request)}))
    except Exception as exc:
        print(json.dumps({"ok":False,"error":str(exc)}))
        raise SystemExit(1)
