"""Quality gates for the frozen, external-label A/B, not merely a successful process exit.
Thresholds apply to the default learned hybrid, not the opt-in experimental reranker.
The split still named heldout is now a regression set: failures have been inspected.
Thresholds are unchanged; a gate failure keeps the PR in draft.
"""
from __future__ import annotations
import json
from pathlib import Path
import sys


def check(report: dict) -> list[str]:
    errors: list[str] = []
    def need(condition, message):
        if not condition: errors.append(message)
    need(report.get('sourceUnchanged') is True, 'frozen source changed or was not checked')
    summary = report.get('summary', {})
    for split, count in [('dev', 8), ('heldout', 12), ('control', 4), ('negative', 2)]:
        for arm in ['A_query', 'A_task', 'B_lexical', 'C_semantic', 'D_rerank']:
            rows = [r for r in report.get('runs', []) if r['split'] == split and r['arm'] == arm]
            need(len({r['id'] for r in rows}) == count, f'{split}/{arm}: expected {count} unique questions')
            need(bool(rows) and all(r.get('ok') for r in rows), f'{split}/{arm}: failed or missing queries')
            counts = [sum(r['id'] == name for r in rows) for name in {r['id'] for r in rows}]
            need(bool(counts) and all(n == report.get('rounds', 0) - 1 for n in counts), f'{split}/{arm}: missing repeated measurements')
    for split, top1, coverage in [('dev', .5, .875), ('heldout', .5, .75), ('all-natural', .6, .8)]:
        result = summary.get(split, {}).get('C_semantic', {})
        need(result.get('top1', 0) >= top1, f'{split}: hybrid top1 must be >= {top1:.0%}')
        need(result.get('coreBody', 0) >= coverage, f'{split}: hybrid core-body coverage must be >= {coverage:.0%}')
        need(result.get('p95Ms', float('inf')) <= 750, f'{split}: warm hybrid p95 must be <= 750ms')
    control = summary.get('control', {})
    baseline = control.get('A_query', {})
    for arm in ['B_lexical', 'C_semantic', 'D_rerank']:
        for metric in ['top1', 'coreBody']:
            need(control.get(arm, {}).get(metric, -1) >= baseline.get(metric, 1), f'{arm}: exact control {metric} regressed')
        need(summary.get('negative', {}).get(arm, {}).get('abstention', 0) == 1, f'{arm}: nonexistent symbols must not return fabricated implementations')
    for arm in ['B_lexical', 'C_semantic']:
        need(summary.get('all-natural', {}).get(arm, {}).get('coreBody', 0) >= summary.get('all-natural', {}).get('A_task', {}).get('coreBody', 1), f'{arm}: natural body coverage regressed vs old task interface')
    need(bool(report.get('firstPass')) and all(r.get('ok') for r in report.get('firstPass', [])), 'first-pass queries failed or were not recorded')
    learned = [r for r in report.get('runs', []) + report.get('firstPass', []) if r['split'] in ('dev', 'heldout') and r['arm'] == 'C_semantic']
    need(bool(learned) and all(r.get('learnedReady') is True for r in learned), 'hybrid measurements must actually use a complete learned index')
    need(report.get('C_semanticPreparation', {}).get('ok') is True, 'learned indexing must complete; preparation cost stays in report')
    return errors

if __name__ == '__main__':
    if len(sys.argv) != 2: raise SystemExit('usage: python scripts/polaris-acceptance.py REPORT.json')
    p = Path(sys.argv[1]); report = json.loads(p.read_text(encoding='utf-8'))
    failures = check(report)
    result = {'passed': not failures, 'failures': failures,
              'scope': '20 Chinese natural-language tasks, 4 exact controls, 2 nonexistent symbols on one frozen repository. Core-body coverage is NOT full-task success.'}
    (p.parent / 'acceptance.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(result, ensure_ascii=False, indent=2))
    raise SystemExit(1 if failures else 0)
