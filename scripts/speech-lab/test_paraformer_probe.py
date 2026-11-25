"""Metric tests need no model, network, or audio device."""
import unittest
from paraformer_probe import contains_term, distance, first_stable_partial, units


class ProbeMetrics(unittest.TestCase):
    def test_mixed_units_and_literal_numbers(self):
        self.assertEqual(units("The 电机: 20 cm."), ["the", "电", "机", "20", "cm"])
        self.assertEqual(distance(units("二十"), units("20")), 2)
        self.assertEqual(distance(units("not finished"), units("finished")), 1)

    def test_english_terms_are_not_substrings(self):
        self.assertFalse(contains_term("another motor", "not"))
        self.assertTrue(contains_term("I have NOT finished.", "not"))
        self.assertTrue(contains_term("还没有完成", "没有"))
        self.assertFalse(contains_term("twenty meters", "twenty centimeters"))

    def test_stable_partial_requires_all_future_updates_and_final(self):
        updates = [{"text": "motor", "at_ms": 200}, {"text": "more", "at_ms": 400},
                   {"text": "motor moves", "at_ms": 600}]
        self.assertEqual(first_stable_partial(updates, "motor moves the car"), 600)
        self.assertIsNone(first_stable_partial(updates, "controller stops"))
        self.assertIsNone(first_stable_partial([], "motor"))


if __name__ == "__main__":
    unittest.main()
