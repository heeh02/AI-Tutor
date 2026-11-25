import unittest
from benchmark import units, distance


class ErrorMetricTests(unittest.TestCase):
    def test_mixed_language_keeps_chinese_even_with_english_speaker(self):
        reference = units("The motor is the 电机.", "zh-en")
        actual = units("THE MOTOR IS THE", "zh-en")
        self.assertEqual(reference, ["the", "motor", "is", "the", "电", "机"])
        self.assertEqual(distance(reference, actual), 2)

    def test_english_word_substitution_and_chinese_char_deletion(self):
        self.assertEqual(distance(units("distance sensor", "en"), units("distance censor", "en")), 1)
        self.assertEqual(distance(units("没有完成", "zh"), units("完成", "zh")), 2)


if __name__ == "__main__":
    unittest.main()
