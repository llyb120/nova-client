"""Gate regressions use invented measurements, never retrieval answers or model output."""
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('gate', Path(__file__).with_name('polaris-acceptance.py'))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
ARMS = ['A_query', 'A_task', 'B_lexical', 'C_semantic', 'D_rerank']

def fixture():
    result = {'sourceUnchanged': True, 'rounds': 3, 'runs': [], 'firstPass': [],
              'summary': {}, 'C_semanticPreparation': {'ok': True}}
    for split, count in [('dev', 8), ('heldout', 12), ('control', 4), ('negative', 2)]:
        result['summary'][split] = {}
        for arm in ARMS:
            result['summary'][split][arm] = {'top1': 1.0, 'coreBody': 1.0, 'p95Ms': 50, 'abstention': 1.0}
            for number in range(count):
                for repeat in range(3):
                    row = {'split': split, 'id': str(number), 'arm': arm, 'round': repeat,
                           'ok': True, 'learnedReady': True}
                    result['firstPass' if repeat == 0 else 'runs'].append(row)
    result['summary']['all-natural'] = copy.deepcopy(result['summary']['dev'])
    return result

class AcceptanceTests(unittest.TestCase):
    def test_empty_or_development_only_is_not_acceptance(self):
        self.assertTrue(gate.check({}))
        value = fixture()
        value['runs'] = [r for r in value['runs'] if r['split'] == 'dev']
        self.assertTrue(gate.check(value))

    def test_all_required_checks_can_pass(self):
        self.assertEqual(gate.check(fixture()), [])

    def test_green_process_does_not_mask_bad_relevance(self):
        value = fixture()
        value['summary']['heldout']['C_semantic']['top1'] = 0.1
        self.assertTrue(gate.check(value))

    def test_changed_source_and_semantic_fallback_are_rejected(self):
        value = fixture()
        value['sourceUnchanged'] = False
        self.assertTrue(gate.check(value))
        value = fixture()
        next(r for r in value['runs'] if r['arm'] == 'C_semantic')['learnedReady'] = False
        self.assertTrue(gate.check(value))

    def test_exact_regressions_and_false_matches_are_rejected(self):
        value = fixture()
        value['summary']['control']['B_lexical']['coreBody'] = 0
        self.assertTrue(gate.check(value))
        value = fixture()
        value['summary']['negative']['C_semantic']['abstention'] = 0
        self.assertTrue(gate.check(value))

    def test_missing_repeat_failed_first_pass_and_bad_latency_are_rejected(self):
        value = fixture()
        value['runs'].pop()
        self.assertTrue(gate.check(value))
        value = fixture()
        value['firstPass'][0]['ok'] = False
        self.assertTrue(gate.check(value))
        value = fixture()
        value['summary']['all-natural']['C_semantic']['p95Ms'] = 3000
        self.assertTrue(gate.check(value))

if __name__ == '__main__':
    unittest.main()
