"""Pure framing/statistics tests; never spawn a main or open a device."""
import json
import struct
import unittest

from camera_stream_smoke import METHOD, parse_packet, distribution, SmokeFailure


class CameraPacketTests(unittest.TestCase):
    def setUp(self):
        self.method = json.loads(METHOD.read_text(encoding="utf-8"))

    def packet(self, **changes):
        meta = {"protocol_version": 1, "delivery_id": 2, "message_type": "frame", "jpeg_length": 3,
                "frame_id": 7, "camera_session_id": "epoch", "width": 2, "height": 2,
                "frame_age_origin": "after_camera_frame_return"}
        meta.update(changes)
        encoded = json.dumps(meta).encode()
        return struct.pack(">I", len(encoded)) + encoded + b"abc"

    def test_exact_binary_lengths_and_increasing_delivery(self):
        meta, jpeg = parse_packet(self.packet(), self.method, 1)
        self.assertEqual((meta["frame_id"], jpeg), (7, b"abc"))
        for packet in (self.packet(jpeg_length=4), self.packet(protocol_version=2),
                       self.packet(delivery_id=1), self.packet(delivery_id=True), self.packet()[:-1]):
            with self.assertRaises(SmokeFailure):
                parse_packet(packet, self.method, 1)

    def test_state_has_no_pixels_or_required_frame_stamp(self):
        meta = {"protocol_version": 1, "delivery_id": 1, "message_type": "state", "jpeg_length": 0}
        encoded = json.dumps(meta).encode()
        self.assertEqual(parse_packet(struct.pack(">I", len(encoded)) + encoded, self.method, 0)[1], b"")
        with self.assertRaises(SmokeFailure):
            parse_packet(self.packet(message_type="state"), self.method, 0)

    def test_frame_bounds_and_clock_origin_are_not_inferred(self):
        for packet in (self.packet(width=0), self.packet(width=100000, height=100000), self.packet(frame_id=0),
                       self.packet(frame_age_origin="sensor"), self.packet(camera_session_id="x" * 129)):
            with self.assertRaises(SmokeFailure):
                parse_packet(packet, self.method, 0)

    def test_nearest_rank_denominator_excludes_nonfinite_and_boolean(self):
        result = distribution([1, 2, 3, 4, float("nan"), True])
        self.assertEqual(result["count"], 4)
        self.assertEqual((result["p50"], result["p95"], result["p99"]), (2, 4, 4))
        self.assertEqual(distribution([]), {"count": 0})


if __name__ == "__main__":
    unittest.main()
