"""Read-only resources for the exact diagnostic child tree and aggregate GPU.

The Windows process tree excludes WSL Qwen RAM. GPU memory is whole-device,
including unrelated activity; no process names, command lines or tokens are read.
"""
import json
import math
import shutil
import subprocess
import time

import psutil


def gpu_rows(text):
    rows = []
    for line in text.strip().splitlines():
        values = [float(v.strip()) for v in line.split(',')]
        if len(values) != 3 or not all(math.isfinite(v) and v >= 0 for v in values):
            raise ValueError('invalid GPU observation')
        used, total, utilization = values
        if total <= 0 or used > total or utilization > 100:
            raise ValueError('GPU observation out of range')
        rows.append({'memory_used_mib': used, 'memory_total_mib': total, 'gpu_utilization_percent': utilization})
    if not rows:
        raise ValueError('missing GPU observation')
    return rows


class ResourceSampler:
    def __init__(self, child, output):
        self.process = psutil.Process(child.pid)
        self.created = self.process.create_time()
        self.started = time.monotonic()
        self.last_tree = self.last_gpu = -100.0
        self.nvidia = shutil.which('nvidia-smi.exe')
        self.stream = (output / 'resources.jsonl').open('x', encoding='utf-8')
        self.summary = {'scope': 'Whole child lifetime including preparation and cleanup; not active-window-only',
                        'windows_tree_scope': 'Exact child and observed descendants; excludes WSL Qwen RAM',
                        'gpu_scope': 'Aggregate NVIDIA device memory/utilization, not exclusive process attribution',
                        'psutil_version': psutil.__version__, 'tree_interval_seconds': 5,
                        'gpu_interval_seconds': 30, 'gpu_available': self.nvidia is not None,
                        'samples': 0, 'tree_samples': 0, 'gpu_samples': 0,
                        'tree_sample_errors': 0, 'gpu_sample_errors': 0,
                        'maximum_windows_tree_rss_bytes': None, 'maximum_gpu_used_mib': None,
                        'maximum_gpu_memory_fraction': None, 'minimum_gpu_total_mib': None,
                        'raw_process_identifiers_saved': False}

    def sample(self):
        elapsed = time.monotonic() - self.started
        if elapsed - self.last_tree < 5:
            return
        self.last_tree = elapsed
        row = {'elapsed_ms': elapsed * 1000, 'observed_unix_ms': time.time() * 1000}
        try:
            if not self.process.is_running() or self.process.create_time() != self.created:
                raise psutil.NoSuchProcess(self.process.pid)
            members = [self.process] + self.process.children(recursive=True)
            rss = cpu = 0
            observed = missing = 0
            for member in members:
                try:
                    memory = member.memory_info()
                    times = member.cpu_times()
                    rss += memory.rss
                    cpu += times.user + times.system
                    observed += 1
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    missing += 1
            row['windows_tree'] = {'observed_processes': observed, 'unavailable_processes': missing,
                                   'rss_bytes': rss, 'cumulative_cpu_seconds_of_observed_members': cpu}
            self.summary['tree_samples'] += 1
            self.summary['maximum_windows_tree_rss_bytes'] = max(self.summary['maximum_windows_tree_rss_bytes'] or 0, rss)
        except (psutil.NoSuchProcess, psutil.AccessDenied, OSError) as error:
            row['tree_sample_error'] = type(error).__name__
            self.summary['tree_sample_errors'] += 1
        if self.nvidia and elapsed - self.last_gpu >= 30:
            self.last_gpu = elapsed
            try:
                completed = subprocess.run(
                    [self.nvidia, '--query-gpu=memory.used,memory.total,utilization.gpu',
                     '--format=csv,noheader,nounits'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                    timeout=3, creationflags=subprocess.CREATE_NO_WINDOW, check=True)
                rows = gpu_rows(completed.stdout.decode('ascii', errors='strict'))
                row['gpus'] = rows
                self.summary['gpu_samples'] += 1
                for gpu in rows:
                    self.summary['maximum_gpu_used_mib'] = max(self.summary['maximum_gpu_used_mib'] or 0,
                                                                gpu['memory_used_mib'])
                    self.summary['maximum_gpu_memory_fraction'] = max(self.summary['maximum_gpu_memory_fraction'] or 0,
                                                                         gpu['memory_used_mib'] / gpu['memory_total_mib'])
                    old = self.summary['minimum_gpu_total_mib']
                    self.summary['minimum_gpu_total_mib'] = min(old if old is not None else gpu['memory_total_mib'],
                                                                 gpu['memory_total_mib'])
            except (OSError, ValueError, UnicodeError, subprocess.SubprocessError) as error:
                row['gpu_sample_error'] = type(error).__name__
                self.summary['gpu_sample_errors'] += 1
        self.stream.write(json.dumps(row) + '\n')
        self.stream.flush()
        self.summary['samples'] += 1

    def close(self, output):
        self.stream.close()
        with (output / 'resources-summary.json').open('x', encoding='utf-8') as stream:
            json.dump(self.summary, stream, indent=2)
            stream.write('\n')
        return self.summary
