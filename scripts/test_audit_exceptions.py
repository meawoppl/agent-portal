import copy
from datetime import date
import importlib.util
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("audit_exceptions", Path(__file__).with_name("check-audit-exceptions.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

CONFIG = {"advisories": {"ignore": ["RUSTSEC-2023-0071"]}}
REGISTRY = {"exceptions": [{"id": "RUSTSEC-2023-0071", "owner": "maintainer", "reason": "EC-only signing", "reviewed_on": date(2026, 10, 21), "review_by": date(2026, 11, 21)}]}


class AuditReviewTests(unittest.TestCase):
    def test_current_and_deadline_day_pass(self):
        for today in [date(2026, 10, 21), date(2026, 11, 21)]:
            module.validate(CONFIG, REGISTRY, today)

    def test_expired_and_future_reviews_fail(self):
        for today in [date(2026, 10, 20), date(2026, 11, 22)]:
            with self.assertRaises(ValueError):
                module.validate(CONFIG, REGISTRY, today)

    def test_undocumented_ignore_and_stale_registry_fail(self):
        for config, registry in [({}, REGISTRY), (CONFIG, {})]:
            with self.assertRaises(ValueError):
                module.validate(config, registry, date(2026, 10, 21))

    def test_duplicate_or_invalid_review_fails(self):
        for field, value in [("owner", ""), ("reason", ""), ("review_by", "2026-11-21"), ("review_by", date(2027, 1, 1))]:
            registry = copy.deepcopy(REGISTRY)
            registry["exceptions"][0][field] = value
            with self.assertRaises(ValueError):
                module.validate(CONFIG, registry, date(2026, 10, 21))
        registry = {"exceptions": REGISTRY["exceptions"] * 2}
        with self.assertRaises(ValueError):
            module.validate(CONFIG, registry, date(2026, 10, 21))


if __name__ == "__main__":
    unittest.main()
