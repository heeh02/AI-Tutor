"""Stage a fresh text-only Demo, or seed two fixed cases through an already-running local API."""
import argparse
import datetime
import http.client
import json
from pathlib import Path
import shutil

# Exact allowlist from scripts/voice-bridge/prepare_conversation_demo.py.
PROVIDER_FILES = ('prompts/tutor_core.md', 'prompts/tutor-agent.md',
    'implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json',
    'runtime/kimi/verification-rust.json')
CASES = (
    ('zh_paper_bridge_ambiguous', '实验约束 · 纸桥 Demo', 'zh',
     '根据这些记录，我们能说跨度更长时纸桥承重就更差吗？还有哪些条件没有确定？'),
    ('en_led_resistor', 'Comparison context · LED Demo', 'en',
     'According to these records, did we measure the current or brightness with the 330-ohm resistor? What is still untested?'))


def require(condition, code):
    if not condition:
        raise ValueError(code)


def write_new(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)


def stage(args):
    source, executable, ui, output = (p.resolve() for p in
        (args.source_root, args.executable, args.ui, args.output))
    require(not output.exists(), 'output_root_must_be_new')
    require(source.is_dir() and executable.is_file() and ui.is_dir(), 'stage_input_missing')
    require((ui / 'index.html').is_file() and (ui / 'assets/tutor_ui_bg.wasm').is_file(), 'built_ui_missing')
    copies = [(executable, Path('bin/ai-tutor.exe'))]
    copies += [(source / name, Path(name)) for name in PROVIDER_FILES]
    for path in sorted(ui.rglob('*')):
        require(not path.is_symlink(), 'ui_links_not_copied')
        if path.is_file():
            # This explicitly supplied build directory is the public UI bundle,
            # including its JSON/web manifests and future 3D asset formats.
            copies.append((path, Path('frontend/dist') / path.relative_to(ui)))
    require(all(path.is_file() and not path.is_symlink() for path, _ in copies), 'provider_or_build_file_missing_or_linked')
    output.mkdir(parents=True, exist_ok=False)
    for path, relative in copies:
        target = output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    write_new(output / 'runtime/teaching-provider-policy.json',
              {'protocol_version': 1, 'provider': 'deepseek_flash', 'thinking': 'non_thinking'})
    write_new(output / 'runtime/tutor-delivery.json',
              {'protocol_version': 1, 'mode': 'segments', 'segment_speech': 'after_display'})
    return {'root': str(output), 'copied_files': len(copies), 'started': False,
            'databases_copied': False, 'credentials_copied': False, 'voice_policies_or_models_copied': False}


def counts(state):
    kimi = state['kimi']['metrics']
    return {'foreground_requests': state['metrics'].get('requests', 0),
            'kimi_requests': kimi.get('requests', 0), 'kimi_consolidations': kimi.get('consolidation_requests', 0),
            'acp_memory_requests': kimi.get('acp_memory_requests', 0), 'kimi_retries': kimi.get('retries', 0)}


def zero_idle(state):
    require(all(value == 0 for value in counts(state).values()), 'fixture_has_model_activity')
    require(state['camera']['state'] == 'closed', 'camera_not_closed')
    require(state['audio']['recording'] is False and state['audio'].get('state') == 'idle', 'audio_not_idle')
    require(state['ptz']['mode'] == 'off' and not state.get('actuator_execution'), 'ptz_not_off')
    require(state['kimi'].get('running', 0) == 0 and not state['kimi']['metrics'].get('active_acp_memory'), 'kimi_busy')
    require(state['teaching'].get('running', 0) == 0 and state['teaching'].get('last_request') is None, 'foreground_model_already_used')


def seed(args):
    root, output = args.root.resolve(), args.output.resolve()
    require(not output.exists(), 'report_must_be_new')
    document = json.loads(args.cases.read_text(encoding='utf-8'))
    require(document.get('namespace') == 'simulator' and document.get('frozen_before_execution') is True, 'frozen_simulator_cases_required')
    selected = []
    for cid, name, language, question in CASES:
        matches = [case for case in document['cases'] if case['case_id'] == cid]
        require(len(matches) == 1 and matches[0]['language'] == language, 'fixed_case_missing_or_changed')
        selected.append((matches[0], name, language, question))
    descriptor = json.loads((root / 'runtime/server.json').read_text(encoding='utf-8'))
    port, token = descriptor.get('port'), descriptor.get('token')
    require(type(port) is int and 1 <= port <= 65535 and isinstance(token, str) and token, 'invalid_local_descriptor')

    def call(method, path, value=None):
        connection = http.client.HTTPConnection('127.0.0.1', port, timeout=10)
        try:
            body = None if value is None else json.dumps(value, ensure_ascii=False).encode('utf-8')
            connection.request(method, path, body, {'Content-Type': 'application/json', 'Cookie': 'ai_tutor_local=' + token})
            response = connection.getresponse()
            raw = response.read(2 * 1024 * 1024 + 1)
            require(response.status == 200 and len(raw) <= 2 * 1024 * 1024, 'local_http_request_failed')
            return json.loads(raw)
        finally:
            connection.close()

    before = call('GET', '/api/state')
    zero_idle(before)
    require(before['learners'] == [], 'seed_requires_empty_new_fixture')
    report = {'at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'root': str(root),
              'status': 'started', 'model_counts_before': counts(before), 'cases': [], 'token_recorded': False}
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open('x', encoding='utf-8') as stream:
        try:
            for case, name, language, question in selected:
                learner = call('POST', '/api/learners', {'name': name, 'language': language, 'namespace': 'simulator', 'help_style': 'guided'})
                lid = learner['learner_id']
                session = call('POST', '/api/sessions', {'learner_id': lid, 'resume': False})
                sid = session['session_id']
                saved = call('POST', f'/api/learners/{lid}/sessions/{sid}/comparisons', case['comparison_request'])
                require(saved['item']['source_type'] == 'simulated' and saved['item']['hardware_verified'] is False, 'unexpected_source_grade')
                require(call('GET', f'/api/learners/{lid}/memories') == [], 'unexpected_automatic_memory')
                report['cases'].append({'case_id': case['case_id'], 'learner_id': lid, 'session_id': sid,
                    'language': language, 'comparison_response': saved, 'question': question})
                zero_idle(call('GET', '/api/state'))
            chosen = report['cases'][0]
            resumed = call('POST', '/api/sessions', {'learner_id': chosen['learner_id'], 'resume': True})
            require(resumed['session_id'] == chosen['session_id'], 'chinese_session_changed')
            after = call('GET', '/api/state')
            zero_idle(after)
            require(counts(after) == counts(before), 'model_count_changed_during_seed')
            report.update(status='seeded', selected_learner_id=chosen['learner_id'], selected_session_id=chosen['session_id'],
                          model_counts_after=counts(after), camera='closed', recording=False, ptz='off')
        except Exception as error:
            report.update(status='failed', error_type=type(error).__name__)
            raise
        finally:
            json.dump(report, stream, ensure_ascii=False, indent=2)
    return {'status': report['status'], 'report': str(output), 'selected_session_id': chosen['session_id']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for name, fields in [('stage', ('source-root', 'executable', 'ui', 'output')), ('seed', ('root', 'cases', 'output'))]:
        command = commands.add_parser(name)
        for field in fields:
            command.add_argument('--' + field, type=Path, required=True)
    args = parser.parse_args()
    try:
        result = stage(args) if args.command == 'stage' else seed(args)
    except Exception as error:
        parser.exit(1, f'Preparation failed ({type(error).__name__}); existing files are not overwritten.\n')
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
