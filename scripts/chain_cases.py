"""Requests and independent invariants for the controlled API profile."""
import json


def cases(profile):
    result = []
    account, contract = profile['account'], profile['contract']
    qi, txhash = profile['qi_address'], profile['utxo_hash']
    state_enabled = profile['context'] == 'zone' and profile['processing_state']

    def add(name, method, params, kind='result', expected=None, has_expected=False, invariant=None):
        request = {'jsonrpc': '2.0', 'id': name, 'method': method, 'params': params}
        result.append({'id': name, 'request': json.dumps(request), 'method': method,
                       'kind': kind, 'expected': expected, 'has_expected': has_expected,
                       'invariant': invariant})

    add('chain', 'quai_chainId', [], expected='0x539', has_expected=True)
    add('height', 'quai_blockNumber', [], expected='0x65' if profile['context'] == 'zone' else '0x0', has_expected=True)
    add('eth-height', 'eth_blockNumber', [], expected='0x65' if profile['context'] == 'zone' else '0x0', has_expected=True)
    add('location', 'quai_nodeLocation', [], expected=['0x0', '0x0'] if profile['context'] == 'zone' else ['0x0'], has_expected=True)
    add('running', 'quai_listRunningChains', [])
    add('expansion', 'quai_getProtocolExpansionNumber', [], expected='0x0', has_expected=True)
    add('version', 'quai_clientVersion', [])
    add('network', 'net_version', [], expected='1337', has_expected=True)
    add('gas-price', 'quai_gasPrice', [], kind='result' if profile['context'] == 'zone' else 'error')
    kind = 'result' if state_enabled else 'error'
    balance = hex(2000000000 if profile['scenario'] == 'canonical' else 3000000000)
    slot = 42 if profile['scenario'] == 'canonical' else 99
    word = '0x' + format(slot, '064x')
    add('balance', 'quai_getBalance', [account, 'latest'], kind, balance, state_enabled)
    add('balance-parent', 'quai_getBalance', [account, '0x64'], kind, hex(1000000000), state_enabled)
    add('balance-hash', 'quai_getBalance', [account, {'blockHash': profile['head_hash'], 'requireCanonical': True}], kind, balance, state_enabled)
    add('qi-balance', 'quai_getBalance', [qi, 'latest'], kind, hex(5000), state_enabled)
    add('qi-historical', 'quai_getBalance', [qi, '0x64'], 'error')
    add('code', 'quai_getCode', [contract, 'latest'], kind, '0x60005460005260206000f3', state_enabled)
    add('storage', 'quai_getStorageAt', [contract, '0x0', 'latest'], kind, word, state_enabled)
    add('storage-empty', 'quai_getStorageAt', [contract, '0x1', 'latest'], kind, '0x' + '00' * 32, state_enabled)
    add('missing-state', 'quai_getBalance', [account, '0x9999'], 'error')
    add('unsupported-tag', 'quai_getBalance', [account, 'finalized'], 'error')
    add('conflicting-selector', 'quai_getBalance', [account, {'blockHash': profile['head_hash'], 'blockNumber': '0x64'}], 'error')
    tx = {'from': account, 'to': contract}
    add('call', 'quai_call', [tx, 'latest'], kind, word, state_enabled)
    add('call-parent', 'quai_call', [tx, '0x64'], kind, '0x' + format(7, '064x'), state_enabled)
    add('revert', 'quai_call', [{'from': account, 'to': profile['reverting_contract']}, 'latest'], 'error', invariant='revert' if state_enabled else None)
    add('estimate', 'quai_estimateGas', [tx], kind, invariant='positive_hex' if state_enabled else None)
    add('access-list', 'quai_createAccessList', [tx], kind, invariant='access_list' if state_enabled else None)
    nonce_kind = 'result' if profile['context'] == 'zone' else 'error'
    nonce = 7 if profile['scenario'] == 'canonical' else 9
    add('nonce', 'quai_getTransactionCount', [account, 'latest'], nonce_kind, hex(nonce), nonce_kind == 'result')
    add('nonce-pending', 'quai_getTransactionCount', [account, 'pending'], nonce_kind, hex(nonce + 1), nonce_kind == 'result')
    add('eth-nonce', 'eth_getTransactionCount', [account, 'latest'], nonce_kind, hex(nonce), nonce_kind == 'result')
    add('eth-nonce-pending', 'eth_getTransactionCount', [account, 'pending'], nonce_kind, hex(nonce + 1), nonce_kind == 'result')
    add('utxo', 'quai_getUTXO', [txhash, '0x0'], invariant='utxo')
    add('utxo-missing', 'quai_getUTXO', [txhash, '0x2'], expected=None, has_expected=True)
    add('outpoints', 'quai_getOutpointsByAddress', [qi], invariant='outpoints')
    add('outpoint-range', 'quai_getOutPointsByAddressAndRange', [qi, '0x64', '0x65'], invariant='range')
    add('range-empty', 'quai_getOutPointsByAddressAndRange', [qi, '0x0', '0x1'], expected={}, has_expected=True)
    add('range-reversed', 'quai_getOutPointsByAddressAndRange', [qi, '0x65', '0x64'], 'error')
    add('range-overflow', 'quai_getOutPointsByAddressAndRange', [qi, '0x0', '0x100000000'], 'error')
    add('range-wrong-ledger', 'quai_getOutPointsByAddressAndRange', [account, '0x64', '0x65'], 'error')
    add('qi-fee-wrong-type', 'quai_estimateFeeForQi', [{}], 'error')
    add('qi-fee-empty-inputs', 'quai_estimateFeeForQi', [{'txType': 2}], 'error')
    qi_tx = {'txType': 2, 'txIn': [{'previousOutPoint': {'txHash': txhash, 'index': '0x0'},
                                 'pubKey': '0x02' + '11' * 32}],
             'txOut': [{'denomination': '0x0', 'address': qi, 'lock': '0x0'}]}
    add('qi-fee', 'quai_estimateFeeForQi', [qi_tx], invariant='positive_hex')
    if state_enabled:
        mutator = profile['mutating_contract']
        add('call-mutating-copy', 'quai_call', [{'from': account, 'to': mutator}, 'latest'], expected='0x' + format(99, '064x'), has_expected=True)
        add('storage-after-call', 'quai_getStorageAt', [mutator, '0x0', 'latest'], expected='0x' + format(7, '064x'), has_expected=True)
        add('call-mutating-repeat', 'quai_call', [{'from': account, 'to': mutator}, 'latest'], expected='0x' + format(99, '064x'), has_expected=True)
        add('qi-locked', 'quai_getLockedBalance', [qi], expected='0x1', has_expected=True)
    add('bad-address', 'quai_getBalance', ['0x01', 'latest'], 'error')
    for index, params in enumerate([[], [account], [account, 'latest', 9], [3, 'latest'], [account, True], {}]):
        add(f'params-{index}', 'quai_getBalance', params, 'error')
    for method in ['eth_chainId', 'eth_clientVersion', 'eth_gasPrice']:
        add(method, method, [], 'error' if method == 'eth_gasPrice' and profile['context'] != 'zone' else 'result')
    for method, args in [('eth_getBalance', [account, 'latest']), ('eth_getCode', [contract, 'latest']),
                         ('eth_getStorageAt', [contract, '0x0', 'latest']), ('eth_call', [tx, 'latest']),
                         ('eth_estimateGas', [tx]), ('eth_createAccessList', [tx])]:
        method_kind = 'result' if method == 'eth_getBalance' and profile['context'] == 'zone' else kind
        add(method, method, args, method_kind)
    if state_enabled:
        add('large-id', 'quai_chainId', [])
        req = json.loads(result[-1]['request']); req['id'] = 9007199254740993123456789
        result[-1]['request'] = json.dumps(req)
        add('fraction-id', 'quai_chainId', [])
        result[-1]['request'] = '{"jsonrpc":"2.0","id":0.123456789012345678901,"method":"quai_chainId","params":[]}'
        batch = [json.loads(result[i]['request']) for i in [0, 9, 19]]
        batch[0]['id'] = batch[1]['id'] = 'duplicate'
        batch.append({'jsonrpc': '2.0', 'method': 'quai_chainId'})
        result.append({'id': 'mixed-batch', 'method': '<batch>', 'request': json.dumps(batch), 'kind': 'batch'})
        result.append({'id': 'notification', 'method': 'quai_chainId', 'request': '{"jsonrpc":"2.0","method":"quai_chainId","params":[]}', 'kind': 'empty'})
    return result


def assert_reference(case, exchange, profile):
    from compat_rpc import decode, same
    if exchange['status'] != 200:
        raise AssertionError(f'{case["id"]}: unexpected reference HTTP status')
    if case['kind'] == 'empty':
        assert not exchange['body'].strip()
        return
    value = decode(exchange['body'])
    if case['kind'] == 'batch':
        assert isinstance(value, list) and len(value) == 3
        return
    assert case['kind'] in value, (case['id'], value)
    assert value.get('error', {}).get('message') != 'method handler crashed', (case['id'], value)
    if case.get('has_expected'):
        assert same(value['result'], case['expected']), (case['id'], value, case['expected'])
    invariant = case.get('invariant')
    if invariant == 'positive_hex':
        assert int(value['result'], 16) > 0
    elif invariant == 'revert':
        assert value['error']['data'] == '0xdeadbeef'
    elif invariant == 'access_list':
        assert int(value['result']['gasUsed'], 16) > 21000
        assert value['result']['accessList'] == [{'address': profile['contract'], 'storageKeys': ['0x' + '00' * 32]}]
    elif invariant == 'utxo':
        assert value['result'] == {'address': profile['qi_address'], 'denomination': '0x7', 'lock': '0x0'}
    elif invariant == 'outpoints':
        assert [v['index'] for v in value['result']] == ['0x0', '0x1']
    elif invariant == 'range':
        assert [v['index'] for v in value['result'][profile['utxo_hash']]] == ['0x0', '0x1']
