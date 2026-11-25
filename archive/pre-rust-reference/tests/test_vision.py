import time
import numpy as np
import pytest
from ai_tutor.vision import CameraService, FaceDetector, display_box, letterbox, unletterbox


@pytest.mark.parametrize("shape", [(720,1280), (1280,720), (300,301), (640,640)])
def test_letterbox_coordinate_roundtrip(shape):
    h, w = shape
    blob, transform = letterbox(np.zeros((h,w,3),np.uint8))
    assert blob.shape == (1,3,640,640)
    original = np.array([[w*.1,h*.2,w*.8,h*.9]],np.float32)
    ratio,left,top = transform
    padded = original * ratio + [left,top,left,top]
    np.testing.assert_allclose(unletterbox(padded,transform,w,h),original,rtol=1e-6)


def test_mirrored_and_original_coordinates():
    box = [.1,.2,.4,.8]
    assert display_box(box) == box
    np.testing.assert_allclose(display_box(box,True),[.6,.2,.9,.8])
    np.testing.assert_allclose(display_box(display_box(box,True),True),box)


def packet(tracks, age=0, epoch="one"):
    return {"tracks": tracks,"captured_at":time.monotonic()-age,"camera_session_id":epoch,
            "frame_id":1,"width":100,"height":100,"jpeg":b"", "fps":15}


def test_selection_is_explicit_not_largest_face():
    service=CameraService()
    service._last=packet([{"track_key":"one:1","box":[.1,.1,.2,.2]},
                          {"track_key":"one:2","box":[.3,.3,.9,.9]}])
    assert service.status()["selected_track_key"] is None
    service.select("one:1")
    assert service.status()["selected_track_key"] == "one:1"
    service.clear()
    assert service.status()["selected_track_key"] is None


def test_stale_frame_and_camera_epoch_reject_selection():
    service=CameraService()
    service._last=packet([{"track_key":"one:1"}],age=1)
    with pytest.raises(ValueError):service.select("one:1")
    service._last=packet([{"track_key":"two:1"}],epoch="two")
    with pytest.raises(ValueError):service.select("one:1")


def test_uncertain_association_rejects_binding():
    service=CameraService()
    service._last=packet([{"track_key":"one:1"}])
    service._last["association_ambiguous"]=True
    with pytest.raises(ValueError):service.select("one:1")


def test_model_integrity_and_face_class():
    detector=FaceDetector()
    assert detector.session.get_outputs()[0].shape == [1,5,8400]
