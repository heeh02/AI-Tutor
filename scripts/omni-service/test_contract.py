import base64
import io
import os
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import numpy as np
import soundfile as sf

from contract import ContractError, normalize_request, wav_bytes
from server import authorized, valid_token
from engine import OFFICIAL_SYSTEM, OmniEngine, output_finished


def request_with(part):
    return {"model": "qwen2_5_omni", "messages": [{"role": "user", "content": [part]}]}


class ContractTests(unittest.TestCase):
    def test_text_and_audio_output_options(self):
        result = normalize_request({
            "model": "qwen2_5_omni", "modalities": ["text", "audio"],
            "audio": {"voice": "Ethan", "format": "wav"}, "max_tokens": 16,
            "messages": [{"role": "system", "content": "Host instructions"}, {"role": "user", "content": "Hello"}],
        })
        self.assertTrue(result["audio_output"])
        self.assertEqual(result["voice"], "Ethan")
        self.assertEqual(result["max_tokens"], 16)

    def test_remote_media_is_rejected_without_fetching(self):
        with self.assertRaises(ContractError) as caught:
            normalize_request(request_with({"type": "image_url", "image_url": {"url": "https://example.com/a.jpg"}}))
        self.assertEqual(caught.exception.code, "invalid_image")

    def test_explicit_image_is_decoded_in_memory(self):
        from PIL import Image
        output = io.BytesIO()
        Image.new("RGB", (2, 2), (30, 80, 120)).save(output, format="JPEG")
        encoded = base64.b64encode(output.getvalue()).decode("ascii")
        result = normalize_request(request_with({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64," + encoded}}))
        self.assertEqual(result["messages"][0]["content"][0]["image"].size, (2, 2))

    def test_wav_is_bounded_and_resampled(self):
        samples = np.zeros(1600, dtype=np.float32)
        output = io.BytesIO()
        sf.write(output, samples, 16000, format="WAV", subtype="PCM_16")
        part = {"type": "input_audio", "input_audio": {"data": base64.b64encode(output.getvalue()).decode("ascii"), "format": "wav"}}
        result = normalize_request(request_with(part))
        self.assertEqual(result["messages"][0]["content"][0]["audio"].size, 1600)

    def test_oversize_audio_is_rejected(self):
        output = io.BytesIO()
        sf.write(output, np.zeros(16000 * 21, dtype=np.float32), 16000, format="WAV", subtype="PCM_16")
        part = {"type": "input_audio", "input_audio": {"data": base64.b64encode(output.getvalue()).decode("ascii"), "format": "wav"}}
        with self.assertRaises(ContractError) as caught:
            normalize_request(request_with(part))
        self.assertEqual(caught.exception.code, "invalid_audio")

    def test_invalid_modalities_do_not_escape_contract_errors(self):
        with self.assertRaises(ContractError):
            normalize_request({"model": "qwen2_5_omni", "modalities": [{}], "messages": []})

    def test_system_media_is_rejected(self):
        with self.assertRaises(ContractError) as caught:
            normalize_request({"model": "qwen2_5_omni", "messages": [{"role": "system", "content": [{"type": "image_url", "image_url": {"url": "https://example.com/private"}}]}]})
        self.assertEqual(caught.exception.code, "invalid_messages")

    def test_waveform_output_is_finite_wav(self):
        encoded = wav_bytes(np.array([0, 0.5, -0.5, 2], dtype=np.float32))
        samples, rate = sf.read(io.BytesIO(encoded), dtype="int16")
        self.assertEqual(rate, 24000)
        self.assertEqual(samples[-1], 32767)

    def test_system_prompt_shape_is_official_for_text_and_audio(self):
        engine = OmniEngine(".")
        text = engine.prepare_messages({"messages": [{"role": "user", "content": "Hello"}], "audio_output": False})
        self.assertEqual(text[0]["content"][0]["text"], OFFICIAL_SYSTEM)
        audio = engine.prepare_messages({"messages": [{"role": "system", "content": "Host rules"}, {"role": "user", "content": "Hello"}], "audio_output": True})
        self.assertEqual(audio[0]["content"][0]["text"], OFFICIAL_SYSTEM)
        self.assertIn("Host rules", audio[1]["content"][0]["text"])
        self.assertEqual(audio[-1]["content"], "Hello")

    def test_finish_reason_requires_eos_when_token_limit_is_reached(self):
        self.assertTrue(output_finished(3, 999, 8, 151645))
        self.assertTrue(output_finished(8, 151645, 8, 151645))
        self.assertFalse(output_finished(8, 999, 8, 151645))

    def test_authentication_is_required_and_compared_exactly(self):
        self.assertTrue(valid_token("x" * 32))
        self.assertFalse(valid_token("x" * 31))
        self.assertFalse(valid_token("密" * 32))
        self.assertFalse(valid_token("x" * 31 + " "))
        with patch.dict(os.environ, {"AI_TUTOR_OMNI_TOKEN": "x" * 32}):
            self.assertTrue(authorized(SimpleNamespace(headers={"authorization": "Bearer " + "x" * 32})))
            self.assertFalse(authorized(SimpleNamespace(headers={"authorization": "Bearer " + "y" * 32})))
        with patch.dict(os.environ, {}, clear=True):
            self.assertFalse(authorized(SimpleNamespace(headers={})))


if __name__ == "__main__":
    unittest.main()
