import copy
import threading

import pytest

from ai_tutor import ptz


def prop(current=0):
    return {"minimum":-130,"maximum":130,"step":1,"current":current,
            "manual_supported":True,"relative_flag":False}


def test_native_ptz_constructor_and_status_never_access_hardware(tmp_path):
    calls=[]
    driver=ptz.NativePTZ(tmp_path,device_path="test",executor=lambda request:calls.append(request))
    assert driver.status()["mode"]=="off"
    assert calls==[]
    with pytest.raises(ValueError):driver.manual_move("pan",1)
    assert calls==[]


@pytest.mark.parametrize("axis,delta",[("zoom",1),("pan",3),("pan",-3),("pan",0),("tilt",True),("pan",1.5)])
def test_invalid_motion_rejected_before_native_process(axis,delta,tmp_path):
    driver=ptz.NativePTZ(tmp_path,device_path="test",executor=lambda request:pytest.fail("Must not execute"))
    with pytest.raises(ValueError):driver.manual_move(axis,delta)


def test_range_alignment_and_absolute_manual_fail_closed():
    assert ptz.validate_step("pan",1,prop())==1
    with pytest.raises(ValueError):ptz.validate_step("pan",1,prop(130))
    with pytest.raises(ValueError):ptz.validate_step("pan",1,{**prop(),"manual_supported":False})
    with pytest.raises(ValueError):ptz.validate_step("pan",1,{**prop(),"relative_flag":True})
    with pytest.raises(ValueError):ptz.validate_step("pan",1,{**prop(),"step":2})
    with pytest.raises(ValueError):ptz.validate_step("pan",1,prop(),target=100)


def test_auto_modes_cannot_be_enabled_even_when_native_manual_supported(tmp_path):
    driver=ptz.NativePTZ(tmp_path,device_path="test")
    for mode in ["external_yolo","vendor_auto"]:
        with pytest.raises(ValueError):driver.set_mode(mode)
    assert driver.status()["external_yolo_enabled"] is False
    assert driver.status()["vendor_auto_state"]=="unknown"


def test_native_transaction_limits_and_sets_only_requested_axis(monkeypatch):
    monkeypatch.setattr(ptz,"calibration_result",lambda root:"NOT_RUN")
    state={"pan":prop(7),"tilt":prop(10)}
    changes=[]
    class Camera:
        def __init__(self,path):pass
        def __enter__(self):return self
        def __exit__(self,*args):pass
        def read(self):return copy.deepcopy(state)
        def set_absolute(self,axis,target):
            changes.append((axis,target));state[axis]["current"]=target
    monkeypatch.setattr(ptz,"_CameraControl",Camera)
    monkeypatch.setattr(ptz.time,"sleep",lambda _:None)
    assert ptz._transaction({"action":"read","device_path":"x"})["properties"]["pan"]["current"]==7
    assert changes==[]
    result=ptz._transaction({"action":"move","device_path":"x","axis":"pan","delta":1})
    assert changes==[("pan",8)]
    assert result["readback_matches"] and result["other_axis_unchanged"]
    with pytest.raises(ValueError):ptz._transaction({"action":"move","device_path":"x","axis":"pan","delta":20})
    assert changes==[("pan",8)]


def test_stop_cancels_generation_and_rejects_future_motion(tmp_path):
    entered,release=threading.Event(),threading.Event()
    errors=[]
    def execute(request):
        if request["action"]=="read":return {"properties":{"pan":prop(),"tilt":prop()}}
        entered.set();release.wait(2)
        return {"readback_matches":True}
    driver=ptz.NativePTZ(tmp_path,device_path="x",executor=execute)
    driver.enable_manual(for_calibration=True)
    def move():
        try:driver.manual_move("pan",1)
        except ValueError as exc:errors.append(str(exc))
    worker=threading.Thread(target=move)
    worker.start()
    assert entered.wait(1)
    result=driver.stop()
    assert result["hardware_stop_command_sent"] is False
    release.set();worker.join(2)
    assert errors and driver.status()["mode"]=="off"
    assert driver.status()["last_motion"] is None
    with pytest.raises(ValueError):driver.manual_move("pan",1)


def test_failed_calibration_disables_every_motion_path_and_stop_sends_no_angle(tmp_path,monkeypatch):
    import json
    (tmp_path/"runtime").mkdir()
    (tmp_path/"runtime"/"ptz_motion_probe.json").write_text(json.dumps({"result":"FAIL"}))
    calls=[]
    def execute(request):
        calls.append(request["action"])
        if request["action"]=="read":return {"properties":{"pan":prop(),"tilt":prop()}}
        pytest.fail("Failed calibration must never reach a physical move")
    driver=ptz.NativePTZ(tmp_path,device_path="x",executor=execute)
    driver.probe()
    assert driver.status()["hardware_read_capable"] is True
    assert driver.status()["low_amplitude_calibration"]=="FAIL"
    for operation in [lambda:driver.enable_manual(),lambda:driver.enable_manual(for_calibration=True),lambda:driver.manual_move("pan",1)]:
        with pytest.raises(ValueError):operation()
    assert driver.close()["hardware_stop_command_sent"] is False
    assert calls==["read"]
    monkeypatch.setattr(ptz,"calibration_result",lambda root:"FAIL")
    with pytest.raises(ValueError,match="frozen"):
        ptz._transaction({"action":"move","device_path":"x","axis":"pan","delta":1})
