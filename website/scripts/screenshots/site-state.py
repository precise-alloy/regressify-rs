"""Writes a local published-site state for the demo deployment target, without deploying.

Usage: python site-state.py <deployment settings json> <workspace output directory>
The settings file is <app data>/config/deployments/<workspace key>.json; its selected target
gets <key>.sites/<target id>.json, the copy the desktop app reads when a target has no
public URL. Catalog and search match their latest local runs (Deployed), checkout carries an
older run (Newer than site), and account is absent (Not deployed). The state only feeds the
suite list and Deploy dialog: never deploy with it, because its file hashes are synthetic.
"""
import hashlib
import json
import os
import sys

settings_path, output = sys.argv[1], sys.argv[2]
settings = json.load(open(settings_path, encoding='utf-8'))
target = next(t for t in settings['targets'] if t['id'] == settings['selected'])
provider = target['provider']
hash_length = {'netlify': 40, 'cloudflareWorkers': 32, 'azureSwa': 64}[provider]


def latest_run(suite):
    runs = json.load(open(os.path.join(output, suite, 'runs.json'), encoding='utf-8'))['runs']
    done = [run for run in runs if run['state'] == 'completed' and run.get('fullSuite')]
    return max(done, key=lambda run: run['completedAt'])['id']


def suite_state(suite, run_id):
    report = f'_snapshot/data/suites/{suite}.json'
    digest = hashlib.sha256(f'{suite}:{run_id}'.encode()).hexdigest()[:hash_length]
    return {
        'summary': {'id': suite, 'total': 1, 'passed': 0, 'failed': 1, 'errors': 0,
                    'report': f'{report}?v=AAAAAAAAAAAAAAAAAAAA'},
        'runId': run_id,
        'files': {report: {'size': 2048, 'hash': digest}},
    }


state = {
    'format': 1, 'dataVersion': 2, 'generator': 'docs fixture', 'provider': provider,
    'deployedAt': '2026-11-09T03:00:00Z', 'fingerprint': 'b' * 64,
    'suites': {
        'catalog': suite_state('catalog', latest_run('catalog')),
        'checkout': suite_state('checkout', 'desktop-00000000000000000000000000000001'),
        'search': suite_state('search', latest_run('search')),
    },
}
stem = os.path.splitext(os.path.basename(settings_path))[0]
directory = os.path.join(os.path.dirname(settings_path), f'{stem}.sites')
os.makedirs(directory, exist_ok=True)
path = os.path.join(directory, f"{target['id']}.json")
with open(path, 'w', encoding='utf-8') as handle:
    json.dump(state, handle)
print(path)
