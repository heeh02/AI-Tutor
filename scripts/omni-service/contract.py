import base64
import binascii
import io

MAX_JSON_BYTES = 16 * 1024 * 1024
MAX_MEDIA_BYTES = 6 * 1024 * 1024
MAX_TEXT_CHARS = 64 * 1024
MAX_AUDIO_SECONDS = 20
MODEL_ID = "qwen2_5_omni"
MODEL_NAME = "Qwen2.5-Omni-7B-GPTQ-Int4"


class ContractError(ValueError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


def require(condition, code, message):
    if not condition:
        raise ContractError(code, message)


def decode_base64(value, code):
    require(isinstance(value, str) and 0 < len(value) <= MAX_MEDIA_BYTES * 2, code, "Invalid or oversized media data")
    try:
        decoded = base64.b64decode(value, validate=True)
    except (binascii.Error, ValueError):
        raise ContractError(code, "Media data is not valid base64") from None
    require(0 < len(decoded) <= MAX_MEDIA_BYTES, code, "Decoded media exceeds its size limit")
    return decoded


def decode_image(url):
    require(isinstance(url, str), "invalid_image", "Image URL must be a string")
    prefix, separator, payload = url.partition(",")
    require(bool(separator) and prefix in {"data:image/jpeg;base64", "data:image/png;base64"}, "invalid_image", "Only inline JPEG or PNG images are accepted")
    from PIL import Image, ImageOps
    Image.MAX_IMAGE_PIXELS = 4_000_000
    try:
        image = Image.open(io.BytesIO(decode_base64(payload, "invalid_image")))
        require(image.format in {"JPEG", "PNG"}, "invalid_image", "Image format does not match its declaration")
        image = ImageOps.exif_transpose(image)
        image.thumbnail((1280, 1280))
        return image.convert("RGB")
    except ContractError:
        raise
    except Exception:
        raise ContractError("invalid_image", "The image could not be decoded safely") from None


def decode_audio(data, format_name):
    require(format_name in {"wav", "wave"}, "invalid_audio", "Only PCM WAV audio is accepted")
    import librosa
    import numpy as np
    import soundfile as sf
    try:
        samples, rate = sf.read(io.BytesIO(decode_base64(data, "invalid_audio")), dtype="float32", always_2d=True)
    except Exception:
        raise ContractError("invalid_audio", "The WAV audio could not be decoded") from None
    require(rate in {16000, 24000, 44100, 48000}, "invalid_audio", "Unsupported audio sample rate")
    require(0 < samples.shape[0] / rate <= MAX_AUDIO_SECONDS, "invalid_audio", f"Audio must be between 0 and {MAX_AUDIO_SECONDS} seconds")
    samples = samples.mean(axis=1)
    require(bool(np.isfinite(samples).all()), "invalid_audio", "Audio contains non-finite samples")
    if rate != 16000:
        samples = librosa.resample(samples, orig_sr=rate, target_sr=16000)
    require(samples.size <= 16000 * MAX_AUDIO_SECONDS + 1, "invalid_audio", "Resampled audio exceeds its bound")
    return samples.astype("float32", copy=False)


def content_parts(content, counters):
    if isinstance(content, str):
        counters["text"] += len(content)
        require(counters["text"] <= MAX_TEXT_CHARS, "text_too_large", "Text exceeds its request bound")
        return content
    require(isinstance(content, list) and 0 < len(content) <= 16, "invalid_content", "Message content must be text or a bounded list")
    result = []
    for part in content:
        require(isinstance(part, dict), "invalid_content", "Invalid content part")
        kind = part.get("type")
        if kind == "text":
            text = part.get("text")
            require(isinstance(text, str), "invalid_content", "Text content must be a string")
            counters["text"] += len(text)
            require(counters["text"] <= MAX_TEXT_CHARS, "text_too_large", "Text exceeds its request bound")
            result.append({"type": "text", "text": text})
        elif kind == "image_url":
            counters["images"] += 1
            require(counters["images"] == 1, "too_many_images", "Submit exactly one explicit image per request")
            source = part.get("image_url")
            require(isinstance(source, dict), "invalid_image", "image_url must be an object")
            result.append({"type": "image", "image": decode_image(source.get("url"))})
        elif kind == "input_audio":
            counters["audios"] += 1
            require(counters["audios"] == 1, "too_many_audios", "Submit exactly one explicit recording per request")
            source = part.get("input_audio")
            require(isinstance(source, dict), "invalid_audio", "input_audio must be an object")
            result.append({"type": "audio", "audio": decode_audio(source.get("data"), source.get("format"))})
        else:
            raise ContractError("unsupported_media", "Only text, an explicit image and an explicit WAV recording are supported")
    return result


def normalize_request(payload):
    require(isinstance(payload, dict), "invalid_request", "JSON body must be an object")
    require(payload.get("model") in {MODEL_ID, MODEL_NAME}, "invalid_model", "The requested local model is not available")
    require(payload.get("stream", False) is False, "stream_not_supported", "This endpoint returns a complete response")
    modalities = payload.get("modalities", ["text"])
    require(isinstance(modalities, list) and all(isinstance(item, str) for item in modalities) and set(modalities) in ({"text"}, {"text", "audio"}), "invalid_modalities", "Modalities must be text or text and audio")
    audio_options = payload.get("audio", {})
    require(isinstance(audio_options, dict), "invalid_audio_options", "Audio options must be an object")
    voice = audio_options.get("voice", "Chelsie")
    require(voice in {"Chelsie", "Ethan"}, "invalid_voice", "Supported voices are Chelsie and Ethan")
    require(audio_options.get("format", "wav") == "wav", "invalid_audio_options", "Only WAV output is supported")
    max_tokens = payload.get("max_tokens", 1024)
    require(isinstance(max_tokens, int) and not isinstance(max_tokens, bool) and 1 <= max_tokens <= 2048, "invalid_max_tokens", "max_tokens must be between 1 and 2048")
    messages = payload.get("messages")
    require(isinstance(messages, list) and 0 < len(messages) <= 16, "invalid_messages", "Messages must contain between 1 and 16 entries")
    counters = {"text": 0, "images": 0, "audios": 0}
    normalized = []
    for message in messages:
        require(isinstance(message, dict) and message.get("role") in {"system", "user", "assistant"}, "invalid_messages", "Invalid message role")
        require(message["role"] != "system" or isinstance(message.get("content"), str), "invalid_messages", "System instructions must be text only")
        normalized.append({"role": message["role"], "content": content_parts(message.get("content"), counters)})
    return {
        "messages": normalized,
        "audio_output": "audio" in modalities,
        "voice": voice,
        "max_tokens": max_tokens,
    }


def wav_bytes(samples, sample_rate=24000):
    import numpy as np
    import soundfile as sf
    samples = np.asarray(samples, dtype=np.float32).reshape(-1)
    if not samples.size or not np.isfinite(samples).all():
        raise RuntimeError("Model returned invalid audio")
    clipped = np.clip(samples, -1.0, 1.0)
    output = io.BytesIO()
    sf.write(output, clipped, sample_rate, format="WAV", subtype="PCM_16")
    return output.getvalue()
