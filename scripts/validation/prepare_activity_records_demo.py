"""Seed an empty, isolated Demo for historical records and continuation UI checks.

Uses public APIs only. No model, microphone, camera or movement requests.
"""
import argparse
import datetime
import http.client
import json
from pathlib import Path

GOAL_QUESTION = (
    "My goal for this activity is to find out whether changing a paper bridge's "
    "span changes how many coins it can hold. Please help me choose the first "
    "thing to confirm before the next test. I have not carried out the longer-span test."
)


def comparison(index, english=False):
    if english:
        return {
            'submission_id': 'activity-records-english-01',
            'source_type': 'simulated',
            'question': 'Does changing the span change how many coins a paper bridge can hold?',
            'changed_variable': 'Distance between the two supports',
            'conditions': [
                {'name': 'Use the same A4 sheet', 'status': 'not_confirmed'},
                {'name': 'Keep folds and coin positions the same', 'status': 'not_confirmed'}],
            'trials': [
                {'label': 'A', 'setting': 'Folded A4 paper bridge with a 15 cm span',
                 'status': 'reported', 'result': 'It did not collapse with 3 coins in one trial. No more coins were added, so maximum capacity is unknown.', 'trial_count': 1},
                {'label': 'B', 'setting': 'A longer span; its length has not been decided',
                 'status': 'not_tested', 'result': None, 'trial_count': None}],
            'pending_plan': {'text': 'Next time I plan to try a longer span. Its length and which conditions to keep are still undecided. This test has not been performed.', 'accepted': True}}
    return {
        'submission_id': f'activity-records-chinese-{index:02d}',
        'source_type': 'simulated',
        'question': f'纸桥跨度比较 · 历史记录 {index:02d}',
        'changed_variable': '两个支撑物之间的跨度',
        'conditions': [
            {'name': '仍用同一张A4纸', 'status': 'not_confirmed'},
            {'name': '折法和硬币摆放位置保持一致', 'status': 'not_confirmed'}],
        'trials': [
            {'label': 'A', 'setting': 'A4纸折成的纸桥，跨度15厘米',
             'status': 'reported', 'result': '放上3枚硬币时没有塌；只试了1次，没有继续加硬币，最大承重未知。', 'trial_count': 1},
            {'label': 'B', 'setting': '跨度更长，具体厘米数尚未确定',
             'status': 'not_tested', 'result': None, 'trial_count': None}],
        'pending_plan': {'text': f'历史记录{index:02d}的计划：下一次试试更长的跨度，长度和沿用哪些条件还没决定。这项测试尚未做。', 'accepted': True}}


def counts(state):
    return {
        'foreground': state['metrics'].get('requests', 0),
        'provider': state['kimi']['metrics'].get('requests', 0),
        'consolidation': state['kimi']['metrics'].get('consolidation_requests', 0),
        'retries': state['kimi']['metrics'].get('retries', 0)}


def check_idle(state):
    assert not any(counts(state).values()), 'seeding must not invoke models'
    assert state['camera']['state'] == 'closed'
    assert state['audio']['recording'] is False
    assert state['ptz']['mode'] == 'off'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root, output = args.root.resolve(), args.output.resolve()
    assert not output.exists(), 'write a new evidence file'
    descriptor = json.loads((root / 'runtime/server.json').read_text(encoding='utf-8'))

    def call(method, path, body=None):
        conn = http.client.HTTPConnection('127.0.0.1', descriptor['port'], timeout=10)
        try:
            data = None if body is None else json.dumps(body, ensure_ascii=False).encode('utf-8')
            conn.request(method, path, data, {'Cookie': 'ai_tutor_local=' + descriptor['token'], 'Content-Type': 'application/json'})
            response = conn.getresponse()
            raw = response.read(2 * 1024 * 1024 + 1)
            assert response.status == 200 and len(raw) <= 2 * 1024 * 1024, f'{method} {path}: {response.status}'
            return json.loads(raw)
        finally:
            conn.close()

    before = call('GET', '/api/state')
    check_idle(before)
    assert before['learners'] == [], 'requires a fresh isolated fixture'
    output.parent.mkdir(parents=True, exist_ok=True)
    report = {'at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'root': str(root), 'status': 'started', 'synthetic_only': True,
              'model_counts_before': counts(before), 'cases': [], 'goal_question': GOAL_QUESTION}
    with output.open('x', encoding='utf-8') as stream:
        try:
            learner = call('POST', '/api/learners', {'name': '续学手记 · Demo', 'language': 'en', 'namespace': 'simulator', 'help_style': 'guided'})
            lid = learner['learner_id']
            report['learner_id'] = lid
            for name, length, english in [('forms_only_history', 12, False), ('goal_and_continuation', 1, True)]:
                session = call('POST', '/api/sessions', {'learner_id': lid, 'resume': False})
                sid = session['session_id']
                case = {'case_id': name, 'session_id': sid, 'activity_id': session['activity_id'], 'comparisons': []}
                report['cases'].append(case)
                for index in range(1, length + 1):
                    body = comparison(index, english)
                    response = call('POST', f'/api/learners/{lid}/sessions/{sid}/comparisons', body)
                    assert response['item']['source_type'] == 'simulated'
                    assert response['item']['hardware_verified'] is False
                    case['comparisons'].append({'request': body, 'response': response})
                state = call('GET', '/api/state')
                check_idle(state)
                assert state['session']['session_id'] == sid and state['session']['turns'] == []
                case['no_chat_messages'] = True
                if not english:
                    case['finished'] = call('POST', f'/api/sessions/{sid}/finish', {})
                    case['memories_after_finish'] = call('GET', f'/api/learners/{lid}/memories')
                    case['comparison_count'] = length
            after = call('GET', '/api/state')
            check_idle(after)
            assert after['session']['session_id'] == report['cases'][1]['session_id']
            assert after['session']['status'] != 'finished'
            report.update(status='seeded', selected_session_id=after['session']['session_id'],
                          model_counts_after=counts(after), camera='closed', recording=False, ptz='off')
        except Exception as error:
            report.update(status='failed', error_type=type(error).__name__, error=str(error))
            raise
        finally:
            json.dump(report, stream, ensure_ascii=False, indent=2)
    print(json.dumps({'status': report['status'], 'report': str(output), 'cases': len(report['cases']), 'new_model_calls': 0}))


if __name__ == '__main__':
    main()
