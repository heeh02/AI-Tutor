"""Control four isolated BGE previews. Never submit teaching turns or retry a slot.

Run serve in an owned/background terminal: it retains Popen until actual main exit.
check-ready/arm/capture/stop operate only on that case's recorded synthetic root.
"""
import argparse
import datetime
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import struct
import subprocess
import time
from types import SimpleNamespace

import prepare_comparison_context_demo as preparation

METHOD = 'runtime/goal-baselines/20261110/bge-runtime/evaluation-method.json'
METHOD_SHA = '6f50ee1da233a7c40b592eacd57b0fd145634224cf6a90886581bd63469ca0eb'
ADDENDUM_SHA = '4ac51cb24c29ea62a2ed6f206324f6b5ecc1e9b0f0bc152fb3ae1da61d471edd'
MODEL_KEY = 'e8ea69aa701cae2e76b2e742053839773a19bbee66e53278864485d98b61bbf0'
CASE_IDS = ('germination_q08', 'germination_q06', 'paper_bridge_q11', 'fractions_q16')


class ControlError(Exception):
    pass


def require(ok, code):
    if not ok:
        raise ControlError(code)


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'), allow_nan=False).encode('utf-8')


def read(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def completed_json(path, deadline):
    # Another owned command may have reserved its create_new evidence file but
    # not finished writing yet. This retries only the evidence read, never an action.
    while True:
        try:
            return read(path)
        except (FileNotFoundError, json.JSONDecodeError):
            require(time.monotonic() < deadline, f'evidence_not_complete:{path}')
            time.sleep(.05)


def observations(path):
    raw = path.read_bytes()
    # The owned observer may currently be writing the last line. Only complete
    # LF-delimited evidence is admissible; never parse a partial receipt.
    end = raw.rfind(b'\n')
    return [] if end < 0 else [json.loads(line) for line in raw[:end].splitlines() if line]


def new_file(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    require(not path.exists(), f'existing_marker:{path}')
    try:
        return path.open('x', encoding='utf-8', newline='\n')
    except FileExistsError:
        raise ControlError(f'existing_marker:{path}') from None


def write_new(path, value):
    with new_file(path) as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())


def failure(error):
    return str(error) if isinstance(error, ControlError) else type(error).__name__


def context(args, staged=True):
    project = args.project_root.resolve(strict=True)
    method_path = project / METHOD
    require(sha(method_path.read_bytes()) == METHOD_SHA, 'frozen_method_changed')
    evidence = method_path.parent
    require(sha((evidence / 'isolation-addendum.json').read_bytes()) == ADDENDUM_SHA, 'isolation_addendum_changed')
    method = read(method_path)
    slot = next(row for row in method['slots'] if row['original_query_id'] == args.case_id)
    require(sha(slot['question_exact'].encode()) == slot['question_utf8_sha256'], 'question_hash_changed')
    case = evidence / 'cases' / args.case_id
    root = (args.root if staged else args.output_root).resolve()
    if staged:
        stage = read(case / 'stage.json')
        require(stage['status'] == 'staged' and Path(stage['root']).resolve() == root, 'root_not_owned_by_this_case')
        require(stage['case_id'] == args.case_id and stage['method_sha256'] == METHOD_SHA, 'stage_binding_mismatch')
        seed = read(case / 'seed.json')
        require(seed['status'] == 'seeded_no_model_or_encoding' and seed['namespace'] == 'simulator'
                and seed['source_count'] == 60 and seed['memory_count'] == 48, 'invalid_synthetic_seed')
    else:
        seed = None
    return SimpleNamespace(project=project, evidence=evidence, case=case, root=root, slot=slot, seed=seed,
                           case_id=args.case_id, language='en' if slot['question_language'] == 'en' else 'zh')


def stage(args):
    c = context(args, False)
    require(not (c.case / 'attempt.json').exists(), f'already_armed:{c.case / "attempt.json"}')
    require(not c.root.exists(), f'output_root_exists:{c.root}')
    executable = c.project / 'target/release/ai-tutor.exe'
    seed_exe = c.project / 'target/release/seed-bge-runtime.exe'
    model = c.project / 'models/memory/bge-m3'
    python = c.project / 'runtime/speech-gpu/omni/bin/python'
    require(executable.is_file() and seed_exe.is_file() and model.is_dir(), 'release_or_model_missing')
    require(python.parent.is_dir(), 'existing_linux_python_environment_missing')
    helpers = [c.project / 'scripts/local-memory' / name for name in ('managed_embedding.py', 'embedding_worker.py')]
    require(all(path.is_file() for path in helpers), 'embedding_helpers_missing')
    record = {'at_utc': utc(), 'status': 'stage_started', 'case_id': c.case_id, 'root': str(c.root),
              'method_sha256': METHOD_SHA, 'isolation_addendum_sha256': ADDENDUM_SHA,
              'query_language': c.slot['question_language'], 'teaching_language': c.language,
              'models_or_services_started': False, 'credentials_copied': False}
    with new_file(c.case / 'stage.json') as report:
        try:
            converted = subprocess.run(['wsl.exe', '--distribution', 'Ubuntu', '--exec', 'wslpath', '-u', str(python)],
                                       stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                       timeout=15, check=False, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            linux_python = converted.stdout.decode('utf-8').rstrip('\r\n')
            require(converted.returncode == 0 and linux_python.startswith('/')
                    and '\n' not in linux_python and '\r' not in linux_python, 'python_wslpath_failed')
            preparation.stage(SimpleNamespace(source_root=c.project, executable=executable, ui=args.ui_build, output=c.root))
            bundle = c.root / 'bin/local-memory'
            bundle.mkdir()
            for helper in helpers:
                shutil.copyfile(helper, bundle / helper.name)
            write_new(c.root / 'runtime/local-memory-policy.json', {'protocol_version': 1, 'mode': 'bge_cpu',
                      'namespace': 'simulator', 'distribution': 'Ubuntu', 'python_executable': linux_python,
                      'model_directory': str(model.resolve())})
            invocation = subprocess.run([str(seed_exe), '--root', str(c.project), '--output-root', str(c.root),
                                         '--report', str(c.case / 'seed.json')], stdin=subprocess.DEVNULL,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120,
                                        check=False, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            require(invocation.returncode == 0, 'seed_failed_no_retry')
            seeded = read(c.case / 'seed.json')
            require(seeded['source_count'] == 60 and seeded['memory_count'] == 48 and seeded['model_calls'] == 0, 'seed_count_mismatch')
            record.update(status='staged', executable_sha256=sha(executable.read_bytes()),
                          seed_executable_sha256=sha(seed_exe.read_bytes()), seed_report_sha256=sha((c.case / 'seed.json').read_bytes()),
                          helper_sha256={p.name: sha(p.read_bytes()) for p in helpers}, source_count=60, memory_count=48,
                          selected_parent_scope=8, unfiltered_48_memory_retrieval=False)
        except Exception as error:
            record.update(status='stage_failed_no_retry', error=failure(error))
            raise
        finally:
            json.dump(record, report, ensure_ascii=False, indent=2)
            report.flush()
    return {'status': record['status'], 'case_id': c.case_id}


class Api:
    def __init__(self, c, pid=None):
        self.c = c
        descriptor = read(c.root / 'runtime/server.json')
        self.port, self.token, self.pid = descriptor['port'], descriptor['token'], descriptor['pid']
        require(type(self.port) is int and 1 <= self.port <= 65535 and isinstance(self.token, str), 'invalid_server_descriptor')
        require(pid is None or self.pid == pid, 'server_pid_mismatch')

    def call(self, method, path, body=None, timeout=15):
        if method != 'GET':
            allowed = path == '/api/sessions' or path == '/api/local-memory/stop' or path.endswith('/settings') or path.endswith('/continue')
            require(method == 'POST' and allowed and not path.endswith('/turns'), 'controller_never_submits_teaching_requests')
        conn = http.client.HTTPConnection('127.0.0.1', self.port, timeout=timeout)
        try:
            payload = None if body is None else json.dumps(body, ensure_ascii=False).encode()
            conn.request(method, path, payload, {'Content-Type': 'application/json', 'Cookie': 'ai_tutor_local=' + self.token})
            response = conn.getresponse()
            data = response.read(8 * 1024 * 1024 + 1)
            require(response.status == 200 and len(data) <= 8 * 1024 * 1024, f'local_api_failed:{method}:{path}:{response.status}')
            return json.loads(data)
        finally:
            conn.close()


def counts(state):
    provider = state['kimi']['metrics']
    return {'foreground': state['metrics'].get('requests', 0), 'provider': provider.get('requests', 0),
            'consolidation': provider.get('consolidation_requests', 0), 'retries': provider.get('retries', 0)}


def safe_state(state, zero=False):
    require(state['camera']['state'] == 'closed' and state['audio']['recording'] is False
            and state['audio']['state'] == 'idle' and state['ptz']['mode'] == 'off'
            and state['ptz']['commands_sent'] == 0 and state['actuator_execution'] is False, 'device_or_audio_activity')
    require(state['tutor_delivery']['automatic_speech'] is False, 'automatic_speech_enabled')
    n = counts(state)
    require(n['consolidation'] == 0 and n['retries'] == 0 and n['foreground'] in (0, 1)
            and n['provider'] in (0, 1), 'foreground_budget_or_retry_violation')
    if zero:
        require(not any(n.values()), 'foreground_already_attempted')


def selected(c, state):
    session = state['session']
    setup = read(c.case / 'setup.json')
    require(session['session_id'] == setup['session_id'] and session['learner_id'] == c.seed['learner_id']
            and session['namespace'] == 'simulator', 'selected_case_identity_changed')
    require(session['continuation']['source_session_id'] == setup['source_session_id'], 'selected_parent_changed')
    return setup


def audit_cache(c, strict=False):
    """Read-only actual vectors and receipt/source validity, never frozen vector import."""
    connection = sqlite3.connect((c.root / 'runtime/data/simulator.sqlite3').as_uri() + '?mode=ro', uri=True)
    connection.row_factory = sqlite3.Row
    try:
        connection.execute('BEGIN')
        lid = c.seed['learner_id']
        epoch = connection.execute('SELECT epoch FROM learners WHERE learner_id=?', (lid,)).fetchone()[0]
        rows = [dict(row) for row in connection.execute('SELECT * FROM memories WHERE learner_id=? AND status=\'active\' ORDER BY memory_id', (lid,))]
        expected = {row['memory_id']: row for row in c.seed['source_mapping'] if row['projected']}
        eligible, indexed, vectors = 0, 0, []
        for memory in rows:
            for key in ('source_event_ids', 'supersedes'):
                memory[key] = json.loads(memory[key])
            if memory['scope'] != 'current_activity' or memory['activity_scope'] != 'modular_rover':
                continue
            links = [dict(row) for row in connection.execute('''SELECT l.event_id,l.event_version FROM memory_source_links l
                JOIN education_event_meta m ON m.event_id=l.event_id AND m.learner_id=l.learner_id AND m.namespace=l.namespace
                AND m.version=l.event_version AND m.status='active'
                JOIN events e ON e.event_id=m.event_id AND e.learner_id=m.learner_id AND e.session_id=m.session_id
                JOIN sessions s ON s.session_id=e.session_id AND s.learner_id=e.learner_id
                WHERE l.memory_id=? AND l.learner_id=? AND l.namespace='simulator' AND l.status='active'
                AND e.source_type='simulated' ORDER BY l.event_id''', (memory['memory_id'], lid))]
            if not links or {row['event_id'] for row in links} != set(memory['source_event_ids']):
                continue
            binding = connection.execute('''SELECT b.activity_id FROM memory_activity_bindings b JOIN sessions s
                ON s.learner_id=b.learner_id AND s.activity_id=b.activity_id WHERE b.memory_id=? AND b.learner_id=?
                AND b.namespace='simulator' LIMIT 1''', (memory['memory_id'], lid)).fetchone()
            if binding is None:
                continue
            eligible += 1
            if strict:
                require(memory['memory_id'] in expected and memory['content'] == expected[memory['memory_id']]['encoding_text'], 'frozen_memory_changed')
            cache = connection.execute('SELECT * FROM memory_embeddings WHERE namespace=\'simulator\' AND learner_id=? AND memory_id=? AND model_key=?',
                                       (lid, memory['memory_id'], MODEL_KEY)).fetchone()
            if cache is None:
                continue
            blob = cache['vector']
            valid = cache['learner_epoch'] == epoch and cache['memory_digest'] == sha(canonical(memory)) and cache['source_digest'] == sha(canonical(links))
            valid = valid and cache['dimension'] == 1024 and len(blob) == 4096
            if valid:
                vector = struct.unpack('<1024f', blob)
                valid = all(math.isfinite(v) for v in vector) and abs(sum(v * v for v in vector) - 1) <= .001
            indexed += int(valid)
            vectors.append({'memory_id': memory['memory_id'], 'valid': valid, 'vector_sha256_f32le': sha(blob),
                            'memory_digest': cache['memory_digest'], 'source_digest': cache['source_digest'], 'dimension': cache['dimension']})
        if strict:
            require(epoch == 0 and len(rows) == 48 and eligible == 48, 'frozen_48_pool_changed')
            for row in c.seed['source_mapping']:
                event = connection.execute('''SELECT e.content,e.source_type,m.kind,m.version,m.status FROM events e
                    JOIN education_event_meta m ON m.event_id=e.event_id WHERE e.event_id=? AND e.learner_id=? AND e.session_id=?''',
                                           (row['event_id'], lid, row['session_id'])).fetchone()
                require(event is not None and event['source_type'] == 'simulated' and event['kind'] == row['original_source']['event_kind']
                        and event['version'] == row['event_version'] and event['status'] == 'active'
                        and json.loads(event['content']) == row['live_imported_event']['content'], 'frozen_source_changed')
        return {'read_only': True, 'epoch': epoch, 'active_memory_count': len(rows), 'eligible': eligible,
                'indexed': indexed, 'model_key': MODEL_KEY, 'vectors': vectors,
                'index_receipts_sha256': sha(canonical(vectors))}
    finally:
        connection.close()


def native_stop(c):
    result = subprocess.run([str(c.root / 'bin/ai-tutor.exe'), '--root', str(c.root), '--stop'],
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            timeout=20, check=False, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    return result.returncode


def serve(args):
    c = context(args)
    require(not (c.case / 'attempt.json').exists(), f'already_armed:{c.case / "attempt.json"}')
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
    record = {'at_utc': utc(), 'status': 'serve_started', 'root': str(c.root), 'case_id': c.case_id, 'port': port}
    with new_file(c.case / 'serve-start.json') as starting, new_file(c.case / 'query-observations.jsonl') as observed:
        child = subprocess.Popen([str(c.root / 'bin/ai-tutor.exe'), '--root', str(c.root), '--no-browser', '--port', str(port)],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        record['pid'] = child.pid
        json.dump(record, starting, indent=2); starting.flush(); os.fsync(starting.fileno())
        last, error, seen = None, None, set()
        try:
            deadline = time.monotonic() + 30
            while True:
                require(child.poll() is None, 'main_exited_before_ready')
                try:
                    api = Api(c, child.pid)
                    last = api.call('GET', '/api/state')
                    break
                except (OSError, ValueError, ControlError):
                    require(time.monotonic() < deadline, 'main_start_timeout_no_restart')
                    time.sleep(.25)
            safe_state(last, True)
            session = api.call('POST', '/api/sessions', {'learner_id': c.seed['learner_id'], 'resume': True})
            require(session['session_id'] == c.seed['selectable_session_id'], 'latest_empty_session_changed')
            api.call('POST', f'/api/sessions/{session["session_id"]}/settings', {'language': c.language})
            state = api.call('GET', '/api/state'); safe_state(state, True)
            source = c.seed['parents'][c.slot['pack_id']]
            continued = api.call('POST', f'/api/learners/{c.seed["learner_id"]}/sessions/{source["session_id"]}/continue',
                                 {'client_submission_id': 'bge-' + c.case_id, 'expected_current_session_id': session['session_id'],
                                  'expected_generation': state['session']['generation']})
            last = api.call('GET', '/api/state'); safe_state(last, True)
            require(last['session']['turns'] == [] and continued['session_id'] == last['session']['session_id'], 'child_not_empty')
            write_new(c.case / 'setup.json', {'at_utc': utc(), 'pid': child.pid, 'port': port, 'session_id': continued['session_id'],
                      'source_session_id': source['session_id'], 'learner_id': c.seed['learner_id'],
                      'query_language': c.slot['question_language'], 'teaching_language': c.language, 'state': last})
            print(json.dumps({'pid': child.pid, 'port': port, 'ready': True}), flush=True)
            while child.poll() is None:
                try:
                    last = api.call('GET', '/api/state', timeout=3)
                    receipt = last.get('local_memory', {}).get('last_query')
                    if receipt and receipt.get('request_id') not in seen:
                        require(last['session']['session_id'] == continued['session_id']
                                and receipt.get('query_text_sha256') == c.slot['question_utf8_sha256'], 'unexpected_query_receipt')
                        observed.write(json.dumps({'at_utc': utc(), 'session_id': continued['session_id'],
                                                   'receipt': receipt}, ensure_ascii=False) + '\n')
                        observed.flush(); os.fsync(observed.fileno()); seen.add(receipt['request_id'])
                except (OSError, ValueError):
                    pass
                time.sleep(.1)
        except BaseException as caught:
            error = failure(caught)
            if child.poll() is None:
                try:
                    Api(c, child.pid).call('POST', '/api/local-memory/stop', {}, timeout=75)
                    last = Api(c, child.pid).call('GET', '/api/state')
                except Exception:
                    pass
                try:
                    native_stop(c)
                except Exception:
                    pass
        finally:
            # Deliberately never terminate/kill a main or WSL process on timeout.
            exit_code = child.wait()
            cleanup = completed_json(c.case / 'stop-observation.json', time.monotonic() + 5) if (c.case / 'stop-observation.json').exists() else None
            write_new(c.case / 'main-exit.json', {'at_utc': utc(), 'pid': child.pid, 'port': port,
                      'main_exit_code': exit_code, 'actual_popen_wait': True, 'error': error,
                      'last_observed_local_memory': None if last is None else last.get('local_memory'),
                      'stop_observation': cleanup})
        require(error is None and exit_code == 0, 'serve_ended_with_recorded_failure')
    return {'pid': child.pid, 'port': port, 'ready': False}


def ready_snapshot(c):
    started = read(c.case / 'serve-start.json')
    api = Api(c, started['pid'])
    state = api.call('GET', '/api/state')
    safe_state(state, True); selected(c, state)
    require(state['session']['turns'] == [], 'child_has_prior_turns')
    cache = audit_cache(c, True)
    local = state['local_memory']
    ok = local.get('phase') == 'active' and local.get('active') is True and local.get('ready') is True
    ok = ok and local.get('coverage') == {'eligible': 8, 'indexed': 8} and cache['indexed'] == 48
    return ok, state, cache, api


def check_ready(args):
    c = context(args)
    deadline = time.monotonic() + args.timeout
    last = {'status': 'waiting', 'at_utc': utc()}
    while True:
        ok, state, cache, _ = ready_snapshot(c)
        last = {'at_utc': utc(), 'status': 'ready' if ok else 'not_ready', 'state': state, 'index': cache}
        if ok:
            write_new(c.case / 'ready.json', last)
            return {'status': 'ready', 'case_id': c.case_id, 'coverage': '8/8', 'indexed': 48}
        if time.monotonic() >= deadline or state['local_memory'].get('phase') in ('degraded', 'configuration_error', 'cleanup_failed', 'stopped'):
            write_new(c.case / 'ready-failed.json', last)
            raise ControlError('ready_deadline_or_runtime_failure_no_restart')
        time.sleep(.5)


def arm(args):
    c = context(args)
    marker = c.case / 'attempt.json'
    require(not marker.exists(), f'already_armed_never_resubmit:{marker}')
    ok, state, cache, api = ready_snapshot(c)
    require(ok, 'case_not_ready_to_arm')
    # Slot order is frozen globally; failed/zero-call captures still cannot be replaced.
    for prior in CASE_IDS[:CASE_IDS.index(c.case_id)]:
        require((c.evidence / 'cases' / prior / 'attempt.json').exists(), 'previous_slot_not_attempted')
    write_new(marker, {'at_utc': utc(), 'case_id': c.case_id, 'root': str(c.root), 'pid': api.pid, 'port': api.port,
              'learner_id': c.seed['learner_id'], 'session_id': state['session']['session_id'],
              'question_exact': c.slot['question_exact'], 'question_utf8_sha256': c.slot['question_utf8_sha256'],
              'query_language': c.slot['question_language'], 'teaching_language': c.language,
              'generation': state['session']['generation'], 'provider_counts_before': counts(state),
              'index_receipts_sha256': cache['index_receipts_sha256'], 'teaching_request_sent_by_controller': False,
              'rule': 'Exactly one browser send after this marker; never re-arm or replace this slot.'})
    return {'status': 'armed', 'case_id': c.case_id, 'question_exact': c.slot['question_exact']}


def capture(args):
    c = context(args)
    record = {'at_utc': utc(), 'case_id': c.case_id, 'root': str(c.root), 'status': 'capture_started',
              'review_scope': 'One isolated slot; independent assistant review is separate.'}
    with new_file(c.case / 'capture.json') as output:
        try:
            api = Api(c, read(c.case / 'serve-start.json')['pid'])
            deadline = time.monotonic() + args.timeout
            while True:
                state = api.call('GET', '/api/state'); safe_state(state)
                record['state'] = state
                if (state['teaching'].get('running', 0) == 0 and state['kimi'].get('running', 0) == 0
                        and state['session']['status'] != 'generating'
                        and (state['session'].get('tutor_stream') or {}).get('phase') not in ('generating', 'final_pending')):
                    break
                require(time.monotonic() < deadline, 'capture_provider_deadline_no_retry')
                time.sleep(.25)
            setup = selected(c, state)
            lid, sid = c.seed['learner_id'], state['session']['session_id']
            record['current_history'] = api.call('GET', f'/api/learners/{lid}/sessions/{sid}/history?limit=50')
            record['parent_history'] = api.call('GET', f'/api/learners/{lid}/sessions/{setup["source_session_id"]}/history?limit=50')
            record['memories'] = api.call('GET', f'/api/learners/{lid}/memories')
            record['index'] = audit_cache(c)
            users = [row for row in record['current_history']['items'] if row['role'] == 'user']
            require(len(users) <= 1 and all(row['content'] == c.slot['question_exact'] for row in users), 'unexpected_case_question')
            rid = users[0]['request_id'] if users else None
            receipt = state['local_memory'].get('last_query')
            origin = 'current_state'
            if rid and (not receipt or receipt.get('request_id') != rid):
                origin = 'observed_while_running_before_epoch_change'
                receipt = next((row['receipt'] for row in observations(c.case / 'query-observations.jsonl')
                                if row['receipt'].get('request_id') == rid), None)
            matched = bool(rid and receipt and receipt.get('request_id') == rid and receipt.get('query_text_sha256') == c.slot['question_utf8_sha256'])
            record.update(provider_counts=counts(state), request_id=rid, matching_query_receipt=receipt if matched else None,
                          receipt_origin=origin if matched else None, receipt_match=matched,
                          status='captured', provider_calls_in_case=counts(state)['provider'])
            record['selected_memory_evidence'] = []
            if matched:
                for mid in receipt.get('selected_memory_ids', []):
                    record['selected_memory_evidence'].append(api.call('GET', f'/api/learners/{lid}/memories/{mid}/evidence'))
            require(counts(state)['foreground'] == 0 or (c.case / 'attempt.json').exists(), 'foreground_without_global_marker')
        except Exception as error:
            record.update(status='capture_failed_no_retry', error=failure(error))
            raise
        finally:
            json.dump(record, output, ensure_ascii=False, indent=2)
    return {'status': record['status'], 'case_id': c.case_id, 'provider_calls': record.get('provider_calls_in_case')}


def stop(args):
    c = context(args)
    started = read(c.case / 'serve-start.json')
    record = {'at_utc': utc(), 'case_id': c.case_id, 'pid': started['pid'], 'status': 'stop_started'}
    error = None
    with new_file(c.case / 'stop-observation.json') as output:
        try:
            api = Api(c, started['pid'])
            api.call('POST', '/api/local-memory/stop', {}, timeout=75)
            state = api.call('GET', '/api/state'); safe_state(state)
            record['local_memory'] = state['local_memory']
            record['provider_counts'] = counts(state)
            owned = state['local_memory'].get('ownership_stop') or {}
            require(state['local_memory'].get('cleanup_confirmed') is True
                    and owned.get('linux_cleanup_confirmed') is True and owned.get('bridge_exited') is True,
                    'owned_helper_cleanup_unconfirmed')
            record['status'] = 'owned_helper_exit_confirmed'
        except Exception as caught:
            error = failure(caught)
            record.update(status='stop_failed', error=error)
        finally:
            # Always ask the native owner to shut down. A failed helper receipt
            # remains a failure even if main subsequently exits normally.
            try:
                record['native_stop_exit_code'] = native_stop(c)
                require(record['native_stop_exit_code'] == 0, 'native_stop_not_accepted')
                if error is None:
                    record['status'] = 'native_shutdown_requested_after_owned_helper_exit'
            except Exception as caught:
                error = error or failure(caught)
                record.update(status='stop_failed', native_stop_error=failure(caught))
            json.dump(record, output, ensure_ascii=False, indent=2)
            output.flush(); os.fsync(output.fileno())
    require(error is None, 'stop_failure_recorded_native_shutdown_attempted')
    deadline = time.monotonic() + args.timeout
    while not (c.case / 'main-exit.json').exists():
        require(time.monotonic() < deadline, 'main_exit_not_yet_observed_do_not_kill')
        time.sleep(.25)
    exited = completed_json(c.case / 'main-exit.json', deadline)
    require(exited['pid'] == started['pid'] and exited['actual_popen_wait'] is True, 'main_exit_binding_mismatch')
    return {'status': 'stopped', 'main_exit_code': exited['main_exit_code'], 'pid': started['pid']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('stage', 'serve', 'check-ready', 'arm', 'capture', 'stop'):
        command = commands.add_parser(name)
        command.add_argument('--project-root', type=Path, required=True)
        command.add_argument('--case-id', choices=CASE_IDS, required=True)
        if name == 'stage':
            command.add_argument('--output-root', type=Path, required=True)
            command.add_argument('--ui-build', type=Path, required=True)
        else:
            command.add_argument('--root', type=Path, required=True)
        if name in ('check-ready', 'capture', 'stop'):
            command.add_argument('--timeout', type=int, default=240 if name == 'check-ready' else 90)
    args = parser.parse_args()
    if hasattr(args, 'timeout'):
        require(1 <= args.timeout <= 600, 'timeout_must_be_1_to_600_seconds')
    try:
        result = globals()[args.command.replace('-', '_')](args)
    except Exception as error:
        parser.exit(1, f'BGE control failed: {failure(error)}\nNo teaching request was sent by this controller; existing evidence is retained.\n')
    print(json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == '__main__':
    main()
