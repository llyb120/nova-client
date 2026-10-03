import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("ab",Path(__file__).with_name("polaris-ab.py"))
ab=importlib.util.module_from_spec(spec);spec.loader.exec_module(ab)
GOLD={"file":"src/jobs.rs","symbol":"cancel_work","needles":["cancel_work(","notify_cancel()"]}
class EvidenceTests(unittest.TestCase):
    def test_metadata_and_mentions_do_not_count(self):
        text='# CTX\n# evidence: src/jobs.rs cancel_work notify_cancel()\n### src/jobs.rs:3-5 [primary wrapper]\n3: fn wrapper() { cancel_work(); notify_cancel(); }\n'
        self.assertFalse(ab.matched(ab.sections(text)[0],GOLD))
    def test_actual_definition_and_body_do_count(self):
        text='### src/jobs.rs:3-5 [primary cancel_work]\n3: fn cancel_work() {\n4: notify_cancel();\n5: }\n'
        self.assertTrue(ab.matched(ab.sections(text)[0],GOLD))
    def test_baseline_code_body_not_impact(self):
        text='### src/jobs.rs (20L) shown=3-5\n@@ 3-5 fn cancel_work\nfn cancel_work() {\n notify_cancel();\n}\n## IMPACT\n### src/other.rs (3L)\n'
        self.assertTrue(ab.matched(ab.sections(text)[0],GOLD))
    def test_relation_body_is_coverage_but_not_primary_rank(self):
        text='### src/ui.rs:1-2 [primary stopButton]\n1: function stopButton() { api.cancelWork(); }\n### src/jobs.rs:3-5 [command-reference cancel_work]\n3: fn cancel_work() { notify_cancel(); }\n'
        m=ab.assess({"primary":[GOLD],"support":[]},text)
        self.assertTrue(m['coreBody']);self.assertFalse(m['top1']);self.assertIsNone(m['rank'])
if __name__=='__main__':unittest.main()
