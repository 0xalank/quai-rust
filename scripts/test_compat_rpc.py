import json
import unittest

from compat_rpc import compare


def response(value, status=200):
    return {'status': status, 'body': json.dumps(value)}


class ComparisonTests(unittest.TestCase):
    def test_object_and_batch_order(self):
        req = '[{"id":1},{"id":2}]'
        values = [{'id': 1, 'result': {'x': 7, 'y': 8}}, {'id': 2, 'result': 9}]
        compare(req, response(values), response(list(reversed(values))))

    def test_duplicate_ids_and_multiplicity(self):
        values = [{'id': 1, 'result': 3}, {'id': 1, 'result': 4}]
        compare('[{},{}]', response(values), response(list(reversed(values))))
        for changed in [values[:1], values + values[:1], [values[0], values[0]]]:
            with self.assertRaises(AssertionError):
                compare('[{},{}]', response(values), response(changed))

    def test_error_message_and_data(self):
        baseline = {'id': 1, 'error': {'code': -32000, 'message': 'insufficient funds', 'data': '0xdeadbeef'}}
        for field, value in [('code', -32001), ('message', 'other'), ('data', '0xdeadbeee')]:
            changed = json.loads(json.dumps(baseline))
            changed['error'][field] = value
            with self.assertRaises(AssertionError):
                compare('{}', response(baseline), response(changed))

    def test_result_array_order_and_duplicates(self):
        baseline = response({'id': 1, 'result': [1, 2, 2]})
        for values in [[2, 1, 2], [1, 2], [1, 2, 2, 2]]:
            with self.assertRaises(AssertionError):
                compare('{}', baseline, response({'id': 1, 'result': values}))

    def test_exact_values_ids_and_types(self):
        baseline = response({'id': 9007199254740993123456789, 'result': '0x01'})
        for changed in [{'id': 9007199254740993123456788, 'result': '0x01'},
                        {'id': 9007199254740993123456789, 'result': '0x00'},
                        {'id': '9007199254740993123456789', 'result': '0x01'}]:
            with self.assertRaises(AssertionError):
                compare('{}', baseline, response(changed))
        with self.assertRaises(AssertionError):
            compare('{}', response({'result': True}), response({'result': 1}))

    def test_null_empty_missing_and_extra_fields(self):
        baseline = response({'result': None})
        for value in [{}, {'result': []}, {'result': ''}, {'result': None, 'extra': 1}]:
            with self.assertRaises(AssertionError):
                compare('{}', baseline, response(value))

    def test_decimal_precision(self):
        a = {'status': 200, 'body': '{"id":0.123456789012345678901,"result":1}'}
        b = dict(a, body=a['body'].replace('901', '902'))
        with self.assertRaises(AssertionError):
            compare('{}', a, b)

    def test_notification_exception_is_explicit_and_narrow(self):
        req = '{"jsonrpc":"2.0","method":"quai_chainId"}'
        a, b = {'status': 200, 'body': ''}, {'status': 204, 'body': ''}
        with self.assertRaises(AssertionError):
            compare(req, a, b)
        self.assertEqual(len(compare(req, a, b, notification_status=True)), 1)
        with self.assertRaises(AssertionError):
            compare(req[:-1] + ',"id":1}', a, b, notification_status=True)


if __name__ == '__main__':
    unittest.main()
