import argparse
import asyncio
import base64
import contextlib
import hmac
import json
import logging
import os
from pathlib import Path
import time
import uuid

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, Response
import uvicorn

from contract import ContractError, MAX_JSON_BYTES, MODEL_ID, normalize_request
from engine import OmniEngine

ROOT = Path(__file__).resolve().parents[2]
LOG = logging.getLogger("ai-tutor-omni")
STATE = {"phase": "loading", "ready": False, "error": None}
ENGINE = OmniEngine(ROOT / "models" / "qwen-omni")
INFERENCE_LOCK = asyncio.Lock()


def error_response(status, code, message):
    return JSONResponse(status_code=status, content={"error": {"code": code, "message": message}})


def valid_token(value):
    return len(value) >= 32 and all(33 <= ord(char) <= 126 for char in value)


def authorized(request):
    expected = os.environ.get("AI_TUTOR_OMNI_TOKEN", "")
    provided = request.headers.get("authorization", "")
    return len(expected) >= 32 and hmac.compare_digest(provided, "Bearer " + expected)


async def json_body(request):
    length = request.headers.get("content-length")
    if length and (not length.isdigit() or int(length) > MAX_JSON_BYTES):
        raise ContractError("request_too_large", "JSON request exceeds its size bound")
    body = bytearray()
    async for chunk in request.stream():
        body.extend(chunk)
        if len(body) > MAX_JSON_BYTES:
            raise ContractError("request_too_large", "JSON request exceeds its size bound")
    try:
        return json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise ContractError("invalid_json", "Request body must be valid JSON") from None


def run_inference(request):
    return ENGINE.generate(request)


@contextlib.asynccontextmanager
async def lifespan(app):
    async def start_model():
        try:
            await asyncio.to_thread(ENGINE.load)
            STATE["phase"] = "warming"
            await asyncio.to_thread(ENGINE.warmup)
            STATE.update(phase="ready", ready=True, error=None)
        except Exception:
            STATE.update(phase="error", ready=False, error="model_start_failed")
            LOG.exception("Omni model startup failed")
    task = asyncio.create_task(start_model())
    yield
    task.cancel()


app = FastAPI(title="AI Tutor local Omni engine", docs_url=None, redoc_url=None, lifespan=lifespan)


@app.get("/health")
async def health():
    return {**STATE, "model": MODEL_ID, "local": True}


@app.get("/v1/models")
async def models(request: Request):
    if not authorized(request):
        return error_response(401, "unauthorized", "Local engine authentication required")
    if not STATE["ready"]:
        return error_response(503, "model_not_ready", "The local model is not ready")
    return {"object": "list", "data": [ENGINE.status()]}


@app.post("/v1/chat/completions")
async def completions(request: Request):
    if not authorized(request):
        return error_response(401, "unauthorized", "Local engine authentication required")
    if not STATE["ready"]:
        return error_response(503, "model_not_ready", "The local model is not ready")
    try:
        payload = normalize_request(await json_body(request))
        started = time.monotonic()
        async with INFERENCE_LOCK:
            result = await asyncio.to_thread(run_inference, payload)
        message = {"role": "assistant", "content": result["text"]}
        if result["audio"] is not None:
            message["audio"] = {
                "data": base64.b64encode(result["audio"]).decode("ascii"),
                "format": "wav", "sample_rate": 24000,
                "transcript": result["text"],
            }
        return {
            "id": "omni_" + uuid.uuid4().hex,
            "created": int(time.time()), "model": MODEL_ID,
            "choices": [{"index": 0, "message": message, "finish_reason": "stop" if result["finished"] else "length"}],
            "usage": None,
            "x_ai_tutor": {
                "elapsed_ms": round((time.monotonic() - started) * 1000, 3),
                "local_inference": True,
                "speech_validation": "generated_waveform_only" if result["audio"] else None,
            },
        }
    except ContractError as error:
        return error_response(400, error.code, error.message)
    except Exception:
        LOG.exception("Omni inference failed")
        return error_response(500, "inference_failed", "The local model could not complete this request")


@app.post("/v1/audio/speech")
async def speech(request: Request):
    if not authorized(request):
        return error_response(401, "unauthorized", "Local engine authentication required")
    if not STATE["ready"]:
        return error_response(503, "model_not_ready", "The local model is not ready")
    try:
        body = await json_body(request)
        if not isinstance(body, dict) or not isinstance(body.get("input"), str) or not body["input"].strip() or len(body["input"]) > 8000:
            raise ContractError("invalid_speech_input", "Speech input must contain between 1 and 8000 characters")
        if body.get("model", MODEL_ID) != MODEL_ID:
            raise ContractError("invalid_model", "The requested local model is not available")
        voice = body.get("voice", "Chelsie")
        if voice not in {"Chelsie", "Ethan"}:
            raise ContractError("invalid_voice", "Supported voices are Chelsie and Ethan")
        if body.get("response_format", "wav") != "wav":
            raise ContractError("invalid_audio_options", "Only WAV output is supported")
        source = body["input"].strip()
        payload = {
            "messages": [
                {"role": "user", "content": "只输出“" + source + "”，不要添加其他内容。"},
            ],
            "audio_output": True, "voice": voice,
            "max_tokens": min(2048, max(32, len(source) + 8)),
        }
        async with INFERENCE_LOCK:
            result = await asyncio.to_thread(run_inference, payload)
        if not result["finished"]:
            return error_response(502, "speech_incomplete", "Generated speech text reached its token limit; audio was not returned")
        if " ".join(result["text"].split()) != " ".join(source.split()):
            return error_response(502, "speech_text_mismatch", "Generated text did not match the requested speech; audio was not returned")
        return Response(result["audio"], media_type="audio/wav", headers={
            "X-Speech-Validation": "model_text_match_only",
            "X-Speech-Sample-Rate": "24000",
        })
    except ContractError as error:
        return error_response(400, error.code, error.message)
    except Exception:
        LOG.exception("Omni speech generation failed")
        return error_response(500, "inference_failed", "The local model could not complete speech generation")


def main():
    parser = argparse.ArgumentParser(description="AI Tutor local Qwen Omni service")
    parser.add_argument("--port", type=int, default=18870)
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error("--port must be between 1024 and 65535")
    token = os.environ.get("AI_TUTOR_OMNI_TOKEN", "")
    if not valid_token(token):
        parser.error("AI_TUTOR_OMNI_TOKEN must contain at least 32 ASCII graphic characters")
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning", access_log=False)


if __name__ == "__main__":
    main()
