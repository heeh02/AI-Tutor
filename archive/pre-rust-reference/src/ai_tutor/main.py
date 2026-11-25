from __future__ import annotations

import asyncio
import base64
import json
import os
import re
import secrets
import time
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Literal

import jsonschema
import psutil
from fastapi import FastAPI, HTTPException, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse, JSONResponse, RedirectResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, ConfigDict, Field

from .audio import AudioService
from .store import Store, uid
from .vision import CameraService
from .kimi import KimiProvider
from .context import build_context
from .ptz import NativePTZ

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = json.loads((ROOT / "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json").read_text(encoding="utf-8"))
STAGES = {"goal_setting", "inventory_check", "capability_gap", "build_guidance", "test_preparation", "observation", "comparison", "reflection", "paused", "completed"}


class Input(BaseModel):
    model_config = ConfigDict(extra="forbid")


class LearnerInput(Input):
    name: str = Field(min_length=1, max_length=48)
    language: Literal["zh", "en", "zh-CN", "en-US"] = "zh"
    help_style: Literal["one_step", "direct", "guided"] = "one_step"
    namespace: Literal["real", "simulator"] = "real"


class SessionInput(Input):
    learner_id: str = Field(max_length=80)
    resume: bool = True


class TurnInput(Input):
    text: str = Field(min_length=1, max_length=4000)


class EventInput(Input):
    modules: list[Literal["control", "motor", "distance", "light"]] | None = Field(default=None, max_length=4)
    source_type: Literal["operator_confirmed", "learner_report", "simulated"]
    report: str | None = Field(default=None, max_length=1000)


class CameraInput(Input):
    device_id: str | None = Field(default=None, max_length=1000)


class TrackInput(Input):
    track_key: str = Field(max_length=150)


class RecordInput(Input):
    device_id: int | None = None
    session_id: str = Field(max_length=80)


class DeliveryInput(Input):
    turn_id: str = Field(max_length=80)
    status: Literal["shown"]


class ContentInput(Input):
    content: str = Field(min_length=1, max_length=800)


class SettingsInput(Input):
    language: Literal["zh", "en", "zh-CN", "en-US"]
    help_style: Literal["one_step", "direct", "guided"]


class PlaybackInput(Input):
    session_id: str = Field(max_length=80)
    turn_id: str = Field(max_length=80)


class Workstation:
    def __init__(self, root=ROOT):
        self.root = root
        self.stores = {ns: Store(root / "runtime/data" / f"{ns}.sqlite3", ns) for ns in ("real", "simulator")}
        self.camera = CameraService()
        self.ptz = NativePTZ(root)
        self.audio = AudioService(root)
        self.provider = KimiProvider(root)
        self.current = None
        self.pending = {}
        self.tasks = {}
        self.speech_tasks = set()
        self.coordination = asyncio.Lock()
        self.camera_operations = asyncio.Lock()
        self.last_submit_at = 0.0
        self.errors = []
        self.metrics = {"requests": 0, "cancelled": 0, "failed": 0, "retries": 0, "consolidation_requests": 0, "reported_tokens": None, "last_latency_ms": None}
        self.started = time.monotonic()
        self.shutdown = asyncio.Event()
        self.clients = set()

    def store_for(self, learner_id):
        for store in self.stores.values():
            if any(x["learner_id"] == learner_id for x in store.learners()):
                return store
        raise KeyError("学习者档案不存在")

    def active(self, sid):
        if not self.current or sid != self.current[1]:
            raise ValueError("请先选择该会话，旧会话不能接收新的操作")
        store = self.stores[self.current[0]]
        return store, store.session(sid)

    async def notify(self, kind="state.changed", **data):
        payload = {"type": kind, **data}
        for queue in tuple(self.clients):
            if queue.full():
                queue.get_nowait()
            queue.put_nowait(payload)

    def record_error(self, message):
        self.errors = (self.errors + [{"message": str(message)[:300], "at": time.time()}])[-8:]

    async def cancel_current(self):
        await self.audio.stop_speech()
        if self.current:
            store = self.stores[self.current[0]]
            sid = self.current[1]
            store.bump(sid)
            for request_id, (task, task_sid) in list(self.tasks.items()):
                if task_sid == sid:
                    await self.provider.cancel(request_id)
                    task.cancel()
                    self.metrics["cancelled"] += 1
            self.pending = {k: v for k, v in self.pending.items() if v["session_id"] != sid}

    async def select_session(self, learner_id, resume):
        async with self.coordination:
            await self.cancel_current()
            self.audio.discard_recording()
            store = self.store_for(learner_id)
            session = store.latest_session(learner_id) if resume else None
            session = session or store.create_session(learner_id)
            self.current = (store.namespace, session["session_id"])
            state = session["state"]
            # Reopening a session is a new physical observation period.
            state["modules"] = None
            state["modules_source"] = "unknown"
            state["requires_inventory_confirmation"] = True
            store.checkpoint(session["session_id"], state)
            await self.notify()
            return store.session(session["session_id"])

    def state(self):
        session = self.stores[self.current[0]].session(self.current[1]) if self.current else None
        ptz = self.ptz.status()
        camera = self.camera.status()
        camera["ptz"] = ptz
        persisted = {}
        for store in self.stores.values():
            for key, value in store.run_metrics().items():
                persisted[key] = persisted.get(key, 0) + value
        return {"learners": sum([s.learners() for s in self.stores.values()], []), "session": session,
                "camera": camera, "kimi": self.provider.status(), "audio": self.audio.status(),
                "metrics": dict(self.metrics, persisted_requests=sum(persisted.values()), persisted_runs=persisted, uptime_seconds=round(time.monotonic()-self.started), memory_mb=round(psutil.Process().memory_info().rss/1024**2, 1)),
                "errors": self.errors, "emotion": {"enabled": False}, "actuator_execution": False,
                "ptz": ptz}

    async def submit(self, sid, text):
        async with self.coordination:
            store, session = self.active(sid)
            if session["status"] == "finished":
                raise ValueError("会话已结束，请开启新会话")
            if self.audio.stream is not None:
                raise ValueError("请先松开说话按钮并确认转写")
            if time.monotonic() - self.last_submit_at < 1.5:
                raise ValueError("输入过于频繁，请稍候再发送")
            self.last_submit_at = time.monotonic()
            self.audio.discard_recording()
            await self.cancel_current()
            generation = store.bump(sid, "generating")
            learner = store.learner(session["learner_id"])
            # Explicit deletion runs locally before any further remote context is built.
            if re.fullmatch(r"\s*(?:请)?(?:忘掉|忘记|删除)(?:我的)?(?:全部|所有)(?:记忆|记录|信息)?[。！!\s]*", text):
                store.forget(learner["learner_id"])
                await self.provider.cleanup_owned(learner["learner_id"])
                await self.notify("memory.updated")
                return {"request_id": None, "local_action": "已删除该档案的记忆与活动记录"}
            if re.match(r"^\s*(?:请)?(?:忘掉|忘记|删除).+", text):
                with store.lock, store.db:
                    store.db.execute("UPDATE sessions SET status='idle' WHERE session_id=?", (sid,))
                return {"request_id": None, "local_action": "请在下方“学习记忆”选择具体条目并点“忘记”，或发送“忘记全部记忆”。本次遗忘请求未发送到远程模型。"}
            eid = store.event(learner["learner_id"], sid, "learner_report", {"text": text})
            request_id = uid("request")
            store.add_turn(sid, request_id, generation, "user", text, "shown", [eid])
            state = store.session(sid)["state"]
            state["latest_input"] = text
            if re.search(r"这次|本次|today|this time", text, re.I) and re.search(r"一步|step", text, re.I):
                state["session_preferences"]["help_style"] = "one_step"
            store.checkpoint(sid, state)
            session = store.session(sid)
            try:
                context = build_context(store, session, learner, text, eid, self.camera.status())
            except ValueError:
                with store.lock, store.db:
                    store.db.execute("UPDATE sessions SET status='error' WHERE session_id=?", (sid,))
                raise
            request = {"request_id": request_id, "learner_id": learner["learner_id"], "session_id": sid, "generation": generation, "context": context}
            epoch = learner["epoch"]
            store.start_run(request_id, learner["learner_id"], sid, len(json.dumps(request, ensure_ascii=False)))
            self.metrics["requests"] += 1
            task = asyncio.create_task(self.generate(store, session, request, epoch))
            self.tasks[request_id] = (task, sid)
            await self.notify("tutor.started", request_id=request_id, session_id=sid, generation=generation)
            return {"request_id": request_id, "generation": generation}

    async def generate(self, store, session, request, epoch):
        start = time.monotonic()
        run_status = "cancelled"
        sid, request_id, generation = session["session_id"], request["request_id"], request["generation"]
        try:
            reply = await self.provider.generate(request)
            jsonschema.validate(reply, SCHEMA)
            if reply["request_id"] != request_id:
                raise ValueError("模型回复的请求编号不匹配")
            valid_ids = {e["event_id"] for e in request["context"]["evidence"]}
            if not set(reply["used_evidence_ids"]).issubset(valid_ids):
                raise ValueError("模型回复引用了不存在的证据")
            for candidate in reply["task_update_candidates"] + reply["memory_candidates"]:
                if not set(candidate["source_event_ids"]).issubset(valid_ids):
                    raise ValueError("候选更新缺少本轮可用的证据")
            async with self.coordination:
                current = store.session(sid)
                if self.current != (store.namespace, sid) or current["generation"] != generation or store.learner(current["learner_id"])["epoch"] != epoch:
                    return
                tid = store.add_turn(sid, request_id, generation, "assistant", reply["reply_text"], "generated", reply["used_evidence_ids"])
                self.pending[tid] = {"session_id": sid, "epoch": epoch, "generation": generation, "reply": reply}
                # Pending candidates remain bounded and are applied on actual display.
                while len(self.pending) > 24:
                    self.pending.pop(next(iter(self.pending)))
                store.db.execute("UPDATE sessions SET status='idle' WHERE session_id=?", (sid,))
                store.db.commit()
                await self.notify("tutor.reply", turn_id=tid, session_id=sid, generation=generation, request_id=request_id)
                run_status = "completed"
        except asyncio.CancelledError:
            return
        except Exception as exc:
            run_status = "failed"
            self.metrics["failed"] += 1
            # Provider errors are sanitized by adapter; never send raw stderr to UI.
            message = str(exc)[:240] if not isinstance(exc, jsonschema.ValidationError) else "模型回复未通过结构校验，请重试"
            self.record_error(message)
            with store.lock, store.db:
                if store.session(sid)["generation"] == generation:
                    store.db.execute("UPDATE sessions SET status='error' WHERE session_id=?", (sid,))
            await self.notify("system.error", message=message, session_id=sid, request_id=request_id)
        finally:
            self.metrics["last_latency_ms"] = round((time.monotonic()-start)*1000)
            store.end_run(request_id, run_status, self.metrics["last_latency_ms"])
            self.tasks.pop(request_id, None)

    async def deliver(self, sid, tid):
        async with self.coordination:
            store, session = self.active(sid)
            store.acknowledge(sid, tid, "shown")
            pending = self.pending.pop(tid, None)
            if not pending or pending["generation"] != session["generation"] or pending["epoch"] != store.learner(session["learner_id"])["epoch"]:
                return
            session = store.session(sid)
            reply, state = pending["reply"], session["state"]
            for candidate in reply["task_update_candidates"]:
                field, value = candidate["field"], candidate["value"]
                if field == "stage" and value not in STAGES:
                    continue
                state[field] = value
                sources = store.source_events(session["learner_id"], candidate["source_event_ids"])
                state[field+"_source"] = "simulated" if store.namespace == "simulator" else ("learner_report" if any(s["source_type"] == "learner_report" for s in sources) else "operator_confirmed")
                state["field_evidence"][field] = {"source_event_ids": candidate["source_event_ids"], "status": "model_interpreted_report"}
                if field == "reported_result":
                    state["attempts"] = (state["attempts"] + [{"report": value, "source_event_ids": candidate["source_event_ids"], "hardware_verified": False}])[-12:]
            store.checkpoint(sid, state)
            for candidate in reply["memory_candidates"]:
                if candidate["scope"] == "current_session":
                    if candidate["kind"] == "help_preference":
                        state["session_preferences"]["explicit_request"] = candidate["content"]
                        store.checkpoint(sid, state)
                else:
                    store.add_memory(session["learner_id"], candidate, pending["epoch"])
            await self.notify("memory.updated", session_id=sid)

    async def playback(self, sid, tid):
        store, session = self.active(sid)
        if session["status"] == "generating" or self.audio.stream is not None:
            raise ValueError("请先结束当前输入或生成")
        turn = next((t for t in session["turns"] if t["turn_id"] == tid and t["role"] == "assistant"), None)
        if not turn or turn["status"] not in {"shown", "speech_completed", "speech_interrupted"}:
            raise ValueError("只能播报当前会话已显示的有效回复")
        generation = session["generation"]
        speech_generation = self.audio.speech_generation
        store.acknowledge(sid, tid, "speech_started")
        async def run():
            try:
                if self.current != (store.namespace, sid) or store.session(sid)["generation"] != generation or self.audio.speech_generation != speech_generation:
                    return
                completed = await self.audio.speak(turn["content"], store.learner(session["learner_id"])["language"], expected_generation=speech_generation)
                if self.current == (store.namespace, sid) and store.session(sid)["generation"] == generation:
                    store.acknowledge(sid, tid, "speech_completed" if completed else "speech_interrupted")
            except Exception as exc:
                self.record_error(exc)
            await self.notify("tts.status")
        task = asyncio.create_task(run())
        self.speech_tasks.add(task)
        task.add_done_callback(self.speech_tasks.discard)

    async def close(self):
        await self.cancel_current()
        self.audio.discard_recording()
        await self.audio.close()
        await self.provider.close()
        await asyncio.to_thread(self.ptz.close)
        await asyncio.to_thread(self.camera.close)
        if self.tasks:
            await asyncio.gather(*(t for t, _ in list(self.tasks.values())), return_exceptions=True)
        if self.speech_tasks:
            await asyncio.gather(*list(self.speech_tasks), return_exceptions=True)
        for store in self.stores.values():
            store.close()


def create_app(workstation=None, token=None):
    token = token or os.environ.get("AI_TUTOR_TOKEN") or secrets.token_urlsafe(32)
    ws = workstation or Workstation()

    @asynccontextmanager
    async def lifespan(app):
        await asyncio.gather(ws.audio.probe(), ws.provider.probe(), return_exceptions=True)
        yield
        await ws.close()

    app = FastAPI(title="AI Tutor Local", docs_url=None, redoc_url=None, openapi_url=None, lifespan=lifespan)
    app.state.workstation = ws

    def origin_ok(headers):
        host = headers.get("host", "")
        if not re.fullmatch(r"(?:127\.0\.0\.1|localhost):\d{1,5}", host):
            return False
        origin = headers.get("origin")
        return (not origin or origin == "http://" + host) and headers.get("sec-fetch-site") != "cross-site"

    @app.middleware("http")
    async def local_protection(request: Request, call_next):
        if not origin_ok(request.headers):
            return JSONResponse({"detail": "仅允许本机工作台访问"}, 403)
        if request.url.path == "/" and request.query_params.get("token"):
            if not secrets.compare_digest(request.query_params["token"], token):
                return JSONResponse({"detail": "启动凭证无效，请使用启动脚本打开"}, 401)
            response = RedirectResponse("/", status_code=303)
            response.set_cookie("ai_tutor_local", token, httponly=True, samesite="strict", path="/")
            response.headers["Cache-Control"] = "no-store"
            response.headers["Referrer-Policy"] = "no-referrer"
            return response
        if request.url.path == "/api/health":
            return JSONResponse({"status": "ok", "application": "ai-tutor-local"})
        if not secrets.compare_digest(request.cookies.get("ai_tutor_local", ""), token):
            return JSONResponse({"detail": "请用本项目的启动脚本打开工作台"}, 401)
        length = request.headers.get("content-length", "0")
        if not length.isdigit() or int(length) > 24000 or request.headers.get("transfer-encoding"):
            return JSONResponse({"detail": "请求过大或传输方式不支持"}, 413)
        response = await call_next(request)
        response.headers.update({"Cache-Control": "no-store", "X-Content-Type-Options": "nosniff", "Referrer-Policy": "no-referrer", "X-Frame-Options": "DENY", "Content-Security-Policy": "default-src 'self'; img-src 'self' data: blob:; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'"})
        return response

    @app.exception_handler(ValueError)
    async def bad_input(request, exc):
        return JSONResponse({"detail": str(exc)[:300]}, 400)

    @app.exception_handler(KeyError)
    async def missing(request, exc):
        return JSONResponse({"detail": str(exc)[:300]}, 404)

    @app.get("/api/state")
    async def state():
        return ws.state()

    @app.get("/api/devices/cameras")
    async def cameras():
        return await asyncio.to_thread(ws.camera.discover)

    @app.post("/api/camera/open")
    async def camera_open(data: CameraInput):
        async with ws.camera_operations:
            return await asyncio.to_thread(ws.camera.open, data.device_id)

    @app.post("/api/camera/close")
    async def camera_close():
        async with ws.camera_operations:
            await asyncio.to_thread(ws.camera.close)
        return ws.camera.status()

    @app.get("/api/video/frame")
    async def frame():
        value = ws.camera.latest()
        if not value:
            return {"jpeg": None, **ws.camera.status()}
        return {**value, "jpeg": base64.b64encode(value["jpeg"]).decode() if value.get("jpeg") else None}

    @app.post("/api/tracking/select")
    async def tracking_select(data: TrackInput):
        return ws.camera.select(data.track_key)

    @app.post("/api/tracking/clear")
    async def tracking_clear():
        ws.camera.clear()
        return ws.camera.status()

    @app.post("/api/learners")
    async def create_learner(data: LearnerInput):
        return ws.stores[data.namespace].create_learner(data.name.strip(), data.language, data.help_style)

    @app.post("/api/sessions")
    async def create_session(data: SessionInput):
        return await ws.select_session(data.learner_id, data.resume)

    @app.post("/api/sessions/{sid}/settings")
    async def settings(sid: str, data: SettingsInput):
        async with ws.coordination:
            store, session = ws.active(sid)
            await ws.cancel_current()
            store.settings(session["learner_id"], data.language, data.help_style)
        return {"ok": True}

    @app.post("/api/sessions/{sid}/turns")
    async def turn(sid: str, data: TurnInput):
        text = data.text.strip()
        if not text:
            raise ValueError("请输入内容")
        return await ws.submit(sid, text)

    @app.post("/api/sessions/{sid}/cancel")
    async def cancel(sid: str):
        ws.active(sid)
        async with ws.coordination:
            await ws.cancel_current()
            ws.audio.discard_recording()
        return {"ok": True}

    @app.post("/api/sessions/{sid}/delivery")
    async def delivery(sid: str, data: DeliveryInput):
        await ws.deliver(sid, data.turn_id)
        return {"ok": True}

    @app.get("/api/sessions/{sid}/context")
    async def context(sid: str):
        store, session = ws.active(sid)
        return {"fast_memory": session["state"], "recalled_memories": session["recalled_memories"], "evidence": store.recent_evidence(sid)}

    @app.post("/api/sessions/{sid}/finish")
    async def finish(sid: str):
        store, session = ws.active(sid)
        async with ws.coordination:
            await ws.cancel_current()
            with store.lock, store.db:
                store.db.execute("UPDATE sessions SET status='finished' WHERE session_id=?", (sid,))
        return {"ok": True}

    @app.post("/api/activities/{aid}/events")
    async def task_event(aid: str, data: EventInput):
        if not ws.current:
            raise ValueError("请先选择学习者档案")
        async with ws.coordination:
            store, session = ws.active(ws.current[1])
            if session["activity_id"] != aid:
                raise ValueError("活动已过期")
            if data.source_type == "simulated" and store.namespace != "simulator":
                raise ValueError("请先切换到独立模拟档案")
            await ws.cancel_current()
            eid = store.event(session["learner_id"], session["session_id"], data.source_type, data.model_dump())
            state = store.session(session["session_id"])["state"]
            if data.modules is not None:
                state["modules"] = list(dict.fromkeys(data.modules))
                state["modules_source"] = "simulated" if store.namespace == "simulator" else data.source_type
                state["requires_inventory_confirmation"] = False
                state["field_evidence"]["modules"] = {"source_event_ids": [eid], "hardware_verified": False}
            if data.report:
                state["reported_result"] = data.report
                state["reported_result_source"] = "simulated" if store.namespace == "simulator" else data.source_type
                state["field_evidence"]["reported_result"] = {"source_event_ids": [eid], "hardware_verified": False}
            store.checkpoint(session["session_id"], state)
            return {"event_id": eid, "hardware_verified": False}

    @app.get("/api/learners/{lid}/memories")
    async def memories(lid: str):
        return ws.store_for(lid).memories(lid)

    async def before_forget(lid):
        if ws.current and ws.stores[ws.current[0]].session(ws.current[1])["learner_id"] == lid:
            await ws.cancel_current()
            ws.audio.discard_recording()
        await ws.provider.cleanup_owned(lid)

    @app.patch("/api/learners/{lid}/memories/{mid}")
    async def correct(lid: str, mid: str, data: ContentInput):
        async with ws.coordination:
            await before_forget(lid)
            new_id = ws.store_for(lid).correct_memory(lid, mid, data.content)
        return {"memory_id": new_id}

    @app.delete("/api/learners/{lid}/memories/{mid}")
    async def forget(lid: str, mid: str):
        async with ws.coordination:
            await before_forget(lid)
            ws.store_for(lid).forget(lid, mid)
        return {"ok": True, "context_reset": True}

    @app.delete("/api/learners/{lid}/data")
    async def forget_all(lid: str):
        async with ws.coordination:
            await before_forget(lid)
            ws.store_for(lid).forget(lid)
        return {"ok": True}

    @app.get("/api/audio/devices")
    async def microphones():
        return await asyncio.to_thread(ws.audio.devices)

    @app.post("/api/audio/record/start")
    async def record_start(data: RecordInput):
        async with ws.coordination:
            ws.active(data.session_id)
            await ws.cancel_current()
            return await asyncio.to_thread(ws.audio.start_recording, data.device_id)

    @app.post("/api/audio/record/stop")
    async def record_stop():
        if ws.current:
            store, session = ws.active(ws.current[1])
            language = store.learner(session["learner_id"])["language"]
        else:
            language = "zh"
        return await ws.audio.stop_recording(language)

    @app.post("/api/audio/playback/stop")
    async def playback_stop():
        await ws.audio.stop_speech()
        return {"ok": True}

    @app.post("/api/audio/playback")
    async def playback(data: PlaybackInput):
        await ws.playback(data.session_id, data.turn_id)
        return {"ok": True}

    @app.post("/api/ptz/stop")
    async def ptz_stop():
        return await asyncio.to_thread(ws.ptz.stop)

    @app.post("/api/ptz/enable")
    async def ptz_enable():
        raise HTTPException(409, ws.ptz.status()["reason"])

    @app.post("/api/shutdown")
    async def shutdown():
        ws.shutdown.set()
        return {"ok": True}

    @app.websocket("/api/events")
    async def events(websocket: WebSocket):
        if not origin_ok(websocket.headers) or not secrets.compare_digest(websocket.cookies.get("ai_tutor_local", ""), token):
            await websocket.close(code=1008)
            return
        await websocket.accept()
        queue = asyncio.Queue(maxsize=8)
        ws.clients.add(queue)
        try:
            while True:
                try:
                    event = await asyncio.wait_for(queue.get(), timeout=20)
                except asyncio.TimeoutError:
                    event = {"type": "heartbeat"}
                await websocket.send_json(event)
        except (WebSocketDisconnect, RuntimeError):
            pass
        finally:
            ws.clients.discard(queue)

    if (ROOT / "frontend/dist/assets").exists():
        app.mount("/assets", StaticFiles(directory=ROOT / "frontend/dist/assets"), name="assets")

    @app.get("/")
    async def index():
        path = ROOT / "frontend/dist/index.html"
        if not path.exists():
            raise HTTPException(503, "界面尚未构建，请按 README 完成安装")
        return FileResponse(path)

    return app
