"""One-degree pan-and-restore hardware probe. No tilt or speed commands.

Uses the live authenticated preview in memory for optional numeric scene shift.
Only measurements are retained; no camera image is written to disk.
"""
import base64
import json
from pathlib import Path
import sys
import time
import urllib.request

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/"src"))
from ai_tutor.ptz import NativePTZ


def frame():
    try:
        config=json.loads((ROOT/"runtime"/"server.json").read_text(encoding="utf8"))
        request=urllib.request.Request(f"http://127.0.0.1:{config['port']}/api/video/frame",
                                     headers={"Cookie":"ai_tutor_local="+config["token"]})
        with urllib.request.urlopen(request,timeout=3) as response:
            data=json.load(response)
        if not data.get("jpeg") or data.get("frame_age_ms",10000)>500:
            return None
        image=cv2.imdecode(np.frombuffer(base64.b64decode(data["jpeg"]),np.uint8),cv2.IMREAD_GRAYSCALE)
        return {"image":cv2.resize(image,(640,360)).astype(np.float32),
                "frame_id":data["frame_id"],"epoch":data["camera_session_id"]}
    except Exception:
        return None


def shift(first,second):
    if first is None or second is None or first["epoch"]!=second["epoch"] or first["frame_id"]==second["frame_id"]:
        return {"status":"NOT_RUN","reason":"No fresh matching live preview frames"}
    displacement,response=cv2.phaseCorrelate(first["image"],second["image"],cv2.createHanningWindow((640,360),cv2.CV_32F))
    return {"status":"MEASURED","dx_pixels_at_640_width":round(displacement[0],3),
            "dy_pixels_at_360_height":round(displacement[1],3),"correlation_response":round(response,4),
            "first_frame_id":first["frame_id"],"second_frame_id":second["frame_id"],
            "camera_session_id":first["epoch"]}


def main():
    driver=NativePTZ(ROOT)
    # Never repeat a physical experiment after failed calibration as a routine test.
    existing=ROOT/"runtime"/"ptz_motion_probe.json"
    if existing.exists():
        raise RuntimeError("Physical calibration evidence already exists; this probe will not repeat movement. Review PTZ_REPORT.md and explicitly resolve vendor tracking first.")
    report={"date":"2026-11-01","test":"one_degree_absolute_pan_and_restore",
            "raw_media_saved":False,"tilt_set_calls":0,"continuous_speed_commands":0,
            "physical_visual_verification":"NOT_RUN","vendor_auto_state":"unknown"}
    original=None
    attempted=False
    before=after=restored=None
    try:
        state=driver.probe()
        original=state["capabilities"]["properties"]
        report["original"]=original
        prop=original["pan"]
        if prop["step"] != 1:
            raise ValueError("Probe requires an exact one-degree driver step")
        step=1 if prop["current"]+1<=prop["maximum"] else -1
        driver.enable_manual(for_calibration=True)
        before=frame()
        attempted=True
        report["move"]=driver.manual_move("pan",step)
        time.sleep(.8)
        after=frame()
        report["scene_shift_during_pan"]=shift(before,after)
        report["move_result"]="PASS_DEVICE_READBACK" if report["move"]["readback_matches"] and report["move"]["other_axis_unchanged"] else "FAIL"
    except Exception as exc:
        report["error"]=str(exc)
        report["move_result"]="FAIL"
    finally:
        if attempted and original:
            try:
                current=driver.probe()["capabilities"]["properties"]
                difference=original["pan"]["current"]-current["pan"]["current"]
                if difference:
                    if abs(difference)>2:
                        raise ValueError("Camera changed by more than two degrees; restore refused by hard motion limit")
                    report["restore"]=driver.manual_move("pan",difference,target=original["pan"]["current"])
                else:
                    report["restore"]={"already_at_original":True}
                time.sleep(.8)
                restored=frame()
                final=driver.probe()["capabilities"]["properties"]
                report["final"]=final
                report["restore_result"]="PASS_DEVICE_READBACK" if all(final[a]["current"]==original[a]["current"] for a in ("pan","tilt")) else "FAIL"
                report["scene_shift_after_restore"]=shift(before,restored)
            except Exception as exc:
                report["restore_result"]="FAIL"
                report["restore_error"]=str(exc)
        report["application_stop"]=driver.stop()
        # Readbacks are camera driver evidence; they do not alone prove a physical gimbal moved.
        report["result"]="PASS_DEVICE_READBACK" if report.get("move_result")==report.get("restore_result")=="PASS_DEVICE_READBACK" else "FAIL"
        (ROOT/"runtime"/"ptz_motion_probe.json").write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf8")
        print(json.dumps(report,ensure_ascii=True,indent=2))


if __name__=="__main__":
    main()
