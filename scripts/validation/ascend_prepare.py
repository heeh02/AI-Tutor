"""Prepare exact ASCEND PCM/reference inputs without importing an ASR runtime."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import platform
import re
import shutil
import unicodedata
import wave

import pyarrow
import pyarrow.parquet as pq

from ascend_score import normalize

DATASET_SHA = 'a4c81d2b5ed6124f052089a695972808c16e0ce0c365ec9773c5d1a8fcf043a7'
REVISION = '737e9800ae31be9932ba8464c80366559bd28424'


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def save(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write('\n')


def pcm_bytes(blob):
    if not isinstance(blob, bytes) or blob[:4] != b'RIFF' or blob[8:12] != b'WAVE':
        raise ValueError('expected_embedded_RIFF_WAVE')
    with wave.open(io.BytesIO(blob), 'rb') as source:
        channels, width, rate, count, compression, _ = source.getparams()
        if (channels, width, rate, compression) != (1, 2, 16000, 'NONE'):
            raise ValueError('expected_mono_PCM16LE_16000_without_conversion')
        data = source.readframes(count)
    if not 1600 <= count <= 320000 or len(data) != count * 2:
        raise ValueError('sample_count_or_payload_length_outside_verified_contract')
    return data, count


def prepare(root, output):
    if pyarrow.__version__ != '19.0.1' or platform.python_version() != '3.10.11':
        raise ValueError('use_the_verified_independent_Arrow_environment')
    source = root / 'runtime/goal-baselines/20261104/ascend-test-assets/test-00000-of-00001.parquet'
    prior = root / 'runtime/goal-baselines/20261104/ascend-eval-preparation'
    if digest(source) != DATASET_SHA:
        raise ValueError('dataset_hash_changed')
    final = json.loads((prior / 'preparation-final-manifest.json').read_text(encoding='utf-8'))
    for name, entry in final['files'].items():
        if digest(prior / name) != entry['sha256']:
            raise ValueError('prior_inventory_artifact_changed')
    age_path = root / 'runtime/goal-baselines/20261104/ascend-test-speaker-age-supplement.json'
    age_source = json.loads(age_path.read_text(encoding='utf-8'))
    ages = {item['original_speaker_id']: item['dataset_reported_age_years']
            for item in age_source['test_speakers']}
    if age_source['dataset_revision'] != REVISION or ages != {3: 25, 17: 24}:
        raise ValueError('author_age_supplement_changed')
    frozen_subset = json.loads((prior / 'realtime-subset-draft.json').read_text(encoding='utf-8'))
    selected_ids = [item['id'] for item in frozen_subset['rows']]
    if len(selected_ids) != 12 or len(set(selected_ids)) != 12 or frozen_subset['deficits']:
        raise ValueError('declared_realtime_subset_changed')
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pcm').mkdir()
    rows = []
    ids = set()
    for batch in pq.ParquetFile(source).iter_batches(batch_size=32):
        for row in batch.to_pylist():
            ordinal = len(rows)
            identity = row['id']
            reference = row['transcription']
            if not isinstance(identity, str) or not re.fullmatch(r'[0-9]{5}', identity) or identity in ids:
                raise ValueError('source_id_contract_changed')
            if not isinstance(reference, str) or row['language'] not in ('zh', 'en', 'mixed'):
                raise ValueError('source_text_or_language_contract_changed')
            ids.add(identity)
            blob = row['audio']['bytes']
            pcm, samples = pcm_bytes(blob)
            pcm_name = f'pcm/{ordinal:04d}-{identity}.pcm'
            with (output / pcm_name).open('xb') as stream:
                stream.write(pcm)
            rows.append({'ordinal': ordinal, 'id': identity, 'language': row['language'],
                'anonymous_speaker_id': row['original_speaker_id'], 'session_id': row['session_id'],
                'author_annotated_age_years': ages[row['original_speaker_id']],
                'topic': row['topic'], 'reference': reference,
                'reference_sha256': hashlib.sha256(reference.encode('utf-8')).hexdigest(),
                'reference_uncertain': '[unk]' in reference.lower(),
                'normalized_reference': normalize(reference)[0],
                'source_audio_blob_sha256': hashlib.sha256(blob).hexdigest(),
                'pcm_file': pcm_name, 'pcm_sha256': hashlib.sha256(pcm).hexdigest(),
                'pcm_bytes': len(pcm), 'sample_count': samples, 'sample_rate': 16000,
                'verified_duration_seconds': samples / 16000,
                'duration_metadata_seconds': row['duration']})
    if len(rows) != 1315 or sum(not row['reference_uncertain'] for row in rows) != 1279:
        raise ValueError('full_population_or_reference_partition_changed')
    if not set(selected_ids) <= ids:
        raise ValueError('realtime_id_missing_from_original_test')
    save(output / 'rows.json', rows)
    save(output / 'realtime-subset.json', {'ids': selected_ids,
        'selection_basis': 'Frozen original metadata rule; never ASR output or manual audio quality',
        'source_draft_sha256': digest(prior / 'realtime-subset-draft.json'),
        'source_rows': frozen_subset['rows'], 'reference_uncertain_ids': [
            row['id'] for row in rows if row['id'] in selected_ids and row['reference_uncertain']]})
    copied = {}
    for name in ['ascend_asr_method_v1.json', 'ascend_normalization_v1.json', 'ascend_author_LICENSE.txt']:
        current = Path(__file__).resolve().parent / name
        shutil.copyfile(current, output / name)
        copied[name] = digest(output / name)
    save(output / 'input-manifest.json', {
        'status': 'prepared_without_model_or_devices', 'dataset_repository': 'CAiRE/ASCEND',
        'dataset_revision': REVISION, 'dataset_sha256': DATASET_SHA,
        'dataset_license': 'CC-BY-SA-4.0', 'rows': 1315, 'reference_complete_rows': 1279,
        'reference_uncertain_rows': 36, 'reference_complete_coverage': 1279 / 1315,
        'anonymous_test_speakers': sorted({row['anonymous_speaker_id'] for row in rows}),
        'author_age_supplement_sha256': digest(age_path),
        'author_annotated_age_years': ages,
        'age_scope': 'Historical author annotation24/25; one speaker per age, no current-age or age-effect inference',
        'rows_sha256': digest(output / 'rows.json'),
        'subset_sha256': digest(output / 'realtime-subset.json'), 'copied_sha256': copied,
        'preparer_sha256': digest(Path(__file__)),
        'scorer_sha256': digest(Path(__file__).resolve().parent / 'ascend_score.py'),
        'normalization_config_sha256': digest(output / 'ascend_normalization_v1.json'),
        'python_version': platform.python_version(), 'unicode_database_version': unicodedata.unidata_version,
        'pyarrow_version': pyarrow.__version__, 'audio_transform': 'none; original PCM payload bytes extracted unchanged',
        'models_started': False, 'devices_requested': False,
        'full_input_reference_data_never_sent_as_model_hint': True})
    print(json.dumps({'prepared_rows': len(rows), 'input_manifest_sha256': digest(output / 'input-manifest.json')}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve(strict=True)
    output = args.output.resolve()
    if not output.is_relative_to(root / 'runtime/goal-baselines'):
        raise ValueError('output_outside_isolated_baselines')
    prepare(root, output)


if __name__ == '__main__':
    main()
