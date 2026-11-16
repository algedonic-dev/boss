"""45c7 car2a: synthetic IMAP transport; production transport belongs to2b."""
import base64
import datetime
import hashlib
import contextlib
import imaplib
import importlib.util
import json
from pathlib import Path
import socket
import sys
import threading
import tempfile
import unittest

# These source-only fixtures must not leave binary inputs for tree-wide scanners.
sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name('support_mail.py')
AT = '2026-10-31T21:00:00Z'
SYNTHETIC_PASSWORD = 'synthetic-fixture-only'


def message(identifier='one', body='Whole message body\n', subject='Customer question'):
    return (f'Message-ID: <{identifier}@example.invalid>\r\nSubject: {subject}\r\n'
            'From: Customer <customer@example.invalid>\r\nTo: support@example.invalid\r\n'
            'Date: Fri, 01 Jan 1999 00:00:00 +0000\r\nMIME-Version: 1.0\r\n'
            'Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n'
            + base64.b64encode(body.encode()).decode()+'\r\n').encode()


def row(raw, at=AT):
    return {'received_at': at, 'raw_message_base64': base64.b64encode(raw).decode()}


@contextlib.contextmanager
def standin(messages=(), signed_in=True):
    listener = socket.socket()
    listener.bind(('127.0.0.1', 0))
    listener.listen(1)
    listener.settimeout(5)
    commands, failures = [], []
    port = listener.getsockname()[1]

    def serve():
        try:
            with listener.accept()[0] as connection:
                connection.settimeout(5)
                stream = connection.makefile('rwb', buffering=0)
                stream.write(b'* OK polite synthetic Bridge\r\n')
                authenticated = False
                while True:
                    line = stream.readline(8193)
                    if not line:
                        break
                    if len(line) > 8192:
                        raise ValueError('fixture command exceeded bound')
                    tag, verb, *args = line.decode('ascii').strip().split(' ', 2)
                    verb = verb.upper()
                    commands.append(verb)  # Never retain credential arguments.
                    if verb == 'CAPABILITY':
                        stream.write(f'* CAPABILITY IMAP4rev1\r\n{tag} OK capability\r\n'.encode())
                    elif verb == 'LOGIN':
                        authenticated = signed_in and args == [f'fixture-user "{SYNTHETIC_PASSWORD}"']
                        stream.write(tag.encode()+(b' OK authenticated\r\n' if authenticated else b' NO unavailable\r\n'))
                    elif verb == 'EXAMINE' and authenticated:
                        stream.write(f'* {len(messages)} EXISTS\r\n* OK [UIDVALIDITY 17] stable\r\n{tag} OK [READ-ONLY] selected\r\n'.encode())
                    elif verb == 'UID' and authenticated:
                        if args[0].startswith('SEARCH'):
                            stream.write(('* SEARCH'+''.join(' '+str(i+1) for i in range(len(messages)))+f'\r\n{tag} OK search\r\n').encode())
                        else:
                            index = int(args[0].split()[1])-1
                            raw = messages[index]
                            stream.write(f'* {index+1} FETCH (UID {index+1} INTERNALDATE "31-Oct-2026 21:00:00 +0000" BODY[] {{{len(raw)}}}\r\n'.encode()+raw+b')\r\n'+tag.encode()+b' OK fetch\r\n')
                    elif verb == 'LOGOUT':
                        stream.write(b'* BYE complete\r\n'+tag.encode()+b' OK logout\r\n')
                        break
                    else:
                        stream.write(tag.encode()+b' NO unavailable\r\n')
        except Exception as error:
            failures.append(type(error).__name__)
        finally:
            listener.close()

    task = threading.Thread(target=serve)
    task.start()
    try:
        yield port, commands
    finally:
        task.join(6)
        if task.is_alive():
            listener.close()
            raise AssertionError('fixture lifetime exceeded bound')
        if failures:
            raise AssertionError(f'fixture failed: {failures}')


def mailbox_read(port, password=SYNTHETIC_PASSWORD):
    """Test-only transport, NEVER production TLS/credential-read evidence."""
    with imaplib.IMAP4('127.0.0.1', port, timeout=5) as client:
        try:
            client.login('fixture-user', password)
            client.select('INBOX', readonly=True)
        except imaplib.IMAP4.error:
            return {'reachable': False, 'complete': True, 'messages': []}
        ids = client.uid('SEARCH', None, 'ALL')[1][0].split()
        messages = [row(client.uid('FETCH', uid, '(INTERNALDATE BODY.PEEK[])')[1][0][1]) for uid in ids]
        return {'reachable': True, 'complete': True, 'messages': messages}


class MailSourceContract(unittest.TestCase):
    def module(self):
        self.assertTrue(SCRIPT.is_file(), 'the pure mail source adapter is missing')
        spec = importlib.util.spec_from_file_location('support_mail', SCRIPT)
        value = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(value)
        return value

    def test_review_B1_multipart_body_survives_without_raw_on_reading(self):
        m = self.module()
        plain = 'p' * (10 * 1024)
        prefix = (b'Message-ID: <multipart@example.invalid>\r\nSubject: Real support reply\r\n'
                  b'From: customer@example.invalid\r\nTo: support@example.invalid\r\n'
                  b'MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary="parts"\r\n\r\n'
                  b'--parts\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n'
                  + plain.encode() + b'\r\n--parts\r\nContent-Type: text/html; charset=utf-8\r\n\r\n')
        suffix = b'\r\n--parts--\r\n'
        raw = prefix + b'h' * (80 * 1024 - len(prefix) - len(suffix)) + suffix
        self.assertEqual(len(raw), 80 * 1024)
        output = m.parse_mail_read({'reachable': True, 'complete': True, 'messages': [row(raw)]}, None)
        self.assertEqual(output[0]['payload']['body'], plain)
        self.assertNotIn('refusal', output[0]['payload'])
        self.assertNotIn('raw_message_base64', output[0]['payload'])
        self.assertEqual(output[0]['payload']['source_digest'], 'sha256:' + hashlib.sha256(raw).hexdigest())
        self.assertEqual(output[0]['external_id'], '<multipart@example.invalid>')
        self.assertLess(len(json.dumps(output).encode()), 2 * 1024 * 1024)

    def test_review_M1_oldest_page_and_inclusive_boundary_conserve_later_mail(self):
        m = self.module()
        start = datetime.datetime(2026, 10, 31, 21, tzinfo=datetime.timezone.utc)
        rows = [row(message(f'page-{i:04}'), (start + datetime.timedelta(seconds=i)).isoformat())
                for i in range(257)]
        batch = {'reachable': True, 'complete': True, 'messages': list(reversed(rows))}
        first = m.parse_mail_read(batch, None)
        self.assertEqual(len(first), 256)
        self.assertEqual(first[0]['external_id'], '<page-0000@example.invalid>')
        self.assertEqual(first[-1]['external_id'], '<page-0255@example.invalid>')
        second = m.parse_mail_read(batch, first[-1]['observed_at'])
        self.assertEqual([o['external_id'] for o in second],
                         ['<page-0255@example.invalid>', '<page-0256@example.invalid>'])
        self.assertEqual(m.parse_mail_read({**batch, 'messages': rows}, None), first)

    def test_review_M2_total_bound_defers_valid_siblings_without_refusal(self):
        m = self.module()
        body = 'x' * min(m.MAX_MESSAGE_BYTES // 2, m.MAX_OBSERVATION_JSON_BYTES // 4)
        start = datetime.datetime(2026, 10, 31, 21, tzinfo=datetime.timezone.utc)
        rows = [row(message(f'large-{i:02}', body=body),
                    (start + datetime.timedelta(seconds=i)).isoformat()) for i in range(32)]
        batch = {'reachable': True, 'complete': True, 'messages': rows}
        first = m.parse_mail_read(batch, None)
        self.assertTrue(all(not o['payload'].get('refusal') for o in first),
                        'valid siblings beyond the total bound are deferred, never permanent refusals')
        self.assertLess(len(first), len(rows))
        seen = {o['external_id'] for o in first}
        cursor = first[-1]['observed_at']
        for _ in range(40):
            page = m.parse_mail_read(batch, cursor)
            self.assertTrue(all(o['payload']['body'] == body for o in page))
            self.assertLessEqual(m._json_size(page), m.MAX_READ_JSON_BYTES)
            seen.update(o['external_id'] for o in page)
            if len(seen) == len(rows):
                break
            self.assertGreater(page[-1]['observed_at'], cursor)
            cursor = page[-1]['observed_at']
        self.assertEqual(seen, {f'<large-{i:02}@example.invalid>' for i in range(32)})

    def test_review_L1_address_controls_cannot_reach_jsonb_and_valid_mail_survives(self):
        m = self.module()
        for char in ['\x00', '\x01', '\x1f', '\x7f']:
            raw = message().replace(b'Customer <customer@example.invalid>',
                                    f'Customer <"bad{char}"@example.invalid>'.encode())
            out = m.parse_mail_read({'reachable': True, 'complete': True, 'messages': [row(raw)]}, None)
            for field in ['from', 'to', 'cc']:
                self.assertFalse(any(any(ord(c) < 32 or ord(c) == 127 for c in addr)
                                     for addr in out[0]['payload'][field]))
        good = m.parse_mail_read({'reachable': True, 'complete': True, 'messages': [row(message('valid'))]}, None)
        self.assertEqual(good[0]['payload']['from'], ['customer@example.invalid'])

    def test_review_M2_source_truncated_tie_is_nonpermanent_latch_with_progress_unpaid(self):
        m = self.module()
        raw_rows = [row(message(f'tied-{i}', body='p' * (600 * 1024))) for i in range(3)]
        batch = {'reachable': True, 'complete': True, 'messages': raw_rows}
        # A205615 conditional applies: this source bounds before returning,
        # so no caller-side processed-ID filter can expose the omitted suffix.
        for cursor in [None, AT]:
            with self.assertRaises(m.PollRefused) as error:
                m.parse_mail_read(batch, cursor)
            witness = error.exception.witness
            self.assertEqual({e['message_key'] for e in witness['entries']},
                             {f'<tied-{i}@example.invalid>' for i in range(3)})
            self.assertTrue(all(e['limit']['unit'] == 'bytes' and
                                e['limit']['value'] == m.MAX_READ_JSON_BYTES and
                                e['limit']['observed'] > m.MAX_READ_JSON_BYTES and
                                e['reason'] == 'timestamp_tie_read_byte_bound'
                                for e in witness['entries']))
        # Re-measurement after an existing mailbox-side change, not a new
        # BOSS clearing authority. No message was permanently refused.
        reduced = {**batch, 'messages': raw_rows[:2]}
        self.assertEqual({o['external_id'] for o in m.parse_mail_read(reduced, AT)},
                         {'<tied-0@example.invalid>', '<tied-1@example.invalid>'})

    def test_review_context_consumes_general_window_and_inside_tie_once(self):
        m = self.module()
        cases = [
            [row(message(f'tie-{i}', body='p' * (600 * 1024))) for i in range(3)],
            [row(message(f'first-{i}', body='p' * (500 * 1024))) for i in range(3)] +
            [row(message('later', body='p' * (500 * 1024)), '2026-10-31T21:00:01Z')],
        ]
        def consume(rows):
            cursor = None
            witnessed = []
            processed = []
            for _ in range(5):
                page = m.parse_mail_read({'reachable': True, 'complete': True, 'messages': rows},
                                         cursor, {'witnessed': witnessed})
                self.assertLessEqual(m._json_size(page), m.MAX_READ_JSON_BYTES)
                if not page:
                    break
                processed.extend(o['external_id'] for o in page)
                cursor = max(o['observed_at'] for o in page)
                witnessed = [w for w in witnessed if w['at'] == cursor]
                witnessed.extend({'message_key': o['external_id'], 'at': o['observed_at']}
                                   for o in page if o['observed_at'] == cursor)
            expected = {m._row_details(r['raw_message_base64'])[1] for r in rows}
            self.assertEqual(set(processed), expected)
            self.assertEqual(len(processed), len(expected), 'every message reaches this source-port consumer exactly once')
        for index, rows in enumerate(cases):
            with self.subTest(case=index):
                consume(rows)

    def test_review_read_context_is_optional_closed_and_bounded(self):
        m = self.module()
        batch = {'reachable': True, 'complete': True, 'messages': [row(message())]}
        self.assertEqual(m.parse_mail_read(batch, None), m.parse_mail_read(batch, None, None))
        for context in [{}, {'witnessed': None}, {'witnessed': [], 'other': 1},
                        {'witnessed': [{'message_key': 'x', 'at': AT}] * 65},
                        {'witnessed': [{'message_key': '', 'at': AT}]},
                        {'witnessed': [{'message_key': 'x\x00', 'at': AT}]},
                        {'witnessed': [{'message_key': 'x', 'at': AT, 'other': 1}]},
                        {'witnessed': [{'message_key': 'x', 'at': '2026-10-31T21:00:01Z'}]},
                        {'witnessed': [{'message_key': 'x', 'at': AT}] * 2}]:
            with self.subTest(context=context), self.assertRaises(m.Unreachable):
                m.parse_mail_read(batch, AT, context)
        with self.assertRaises(m.Unreachable):
            m.parse_mail_read(batch, None, {'witnessed': [{'message_key': 'x', 'at': AT}]})

    def test_review_context_overflow_is_nonpermanent_and_remeasured(self):
        m = self.module()
        rows = [row(message(f'tie-{i}')) for i in range(65)]
        batch = {'reachable': True, 'complete': True, 'messages': rows}
        for known in [[], [{'message_key': m._row_details(rows[0]['raw_message_base64'])[1], 'at': AT}]]:
            with self.subTest(known=len(known)), self.assertRaises(m.PollRefused) as error:
                m.parse_mail_read(batch, AT, {'witnessed': known})
            witness = error.exception.witness
            self.assertLessEqual(len(witness['entries']), 64)
            for entry in witness['entries']:
                self.assertEqual(entry['limit'], {'unit': 'count', 'value': 64, 'observed': 65})
                self.assertEqual(entry['reason'], 'cursor_witness_bound')
            reduced = {**batch, 'messages': rows[:64]}
            page = m.parse_mail_read(reduced, AT, {'witnessed': known})
            self.assertEqual(len(page), 64-len(known), 'readable remeasurement preserves all unwitnessed valid mail')

    def test_review_L3_timestamp_tie_names_count_not_an_unexceeded_byte_limit(self):
        m = self.module()
        with self.assertRaises(m.PollRefused) as error:
            m.parse_mail_read({'reachable': True, 'complete': True,
                               'messages': [row(message(str(i))) for i in range(257)]}, None)
        for entry in error.exception.witness['entries']:
            self.assertEqual(entry.get('limit'), {'unit': 'count', 'value': 256, 'observed': 257})
            self.assertNotIn('limit_bytes', entry)

    def test_bad_message_is_named_once_and_does_not_block_following_mail(self):
        m = self.module()
        bad = message('bad', body='bad\x00body')
        good = message('good')
        batch = {'reachable': True, 'complete': True,
                 'messages': [row(bad), row(good, '2026-10-31T21:00:01Z')]}
        output = m.parse_mail_read(batch, None)
        self.assertEqual([o['external_id'] for o in output],
                         ['<bad@example.invalid>', '<good@example.invalid>'])
        self.assertEqual(output[0]['payload']['refusal'], 'body_contains_nul')
        self.assertNotIn('\x00', json.dumps(output))
        self.assertEqual(output[1]['payload']['body'], 'Whole message body\n')
        self.assertEqual(m.parse_mail_read(batch, AT), output)
        self.assertEqual(max(o['observed_at'] for o in output), '2026-10-31T21:00:01Z')

    def test_invalid_message_id_has_stable_digest_refusal_identity(self):
        m = self.module()
        raw = message().replace(b'Message-ID:', b'X-Message-ID:')
        output = m.parse_mail_read({'reachable': True, 'complete': True,
                                    'messages': [row(raw), row(message('good'))]}, None)
        refused = [o for o in output if o['payload'].get('refusal')]
        self.assertEqual(len(refused), 1)
        self.assertTrue(refused[0]['external_id'].startswith('sha256:'))
        self.assertEqual(refused[0]['payload']['refusal'], 'message_id_unavailable')
        self.assertEqual(m.parse_mail_read({'reachable': True, 'complete': True,
                                           'messages': [row(raw)]}, None), refused)

    def test_rfc6532_from_and_two_received_routes_are_one_message(self):
        m = self.module()
        raw = message().replace(b'Customer <customer@example.invalid>',
                                'José García <customer@example.invalid>'.encode())
        first = b'Received: by route-one\r\n' + raw
        second = b'Received: by route-two\r\n' + raw
        def read(rows):
            return m.parse_mail_read({'reachable': True, 'complete': True,
                                      'messages': rows}, None)
        output = read([row(first), row(second)])
        self.assertEqual(len(output), 1)
        self.assertEqual(output[0]['payload']['from'], ['customer@example.invalid'])
        self.assertEqual(read([row(second), row(first)]), output)

    def test_conflicting_content_is_a_named_refusal_not_a_whole_read_failure(self):
        m = self.module()
        rows = [row(message('conflict', body='First')),
                row(message('conflict', body='Contradiction')),
                row(message('good'))]
        def read(values):
            return m.parse_mail_read({'reachable': True, 'complete': True,
                                      'messages': values}, None)
        output = read(rows)
        self.assertEqual(len(output), 2)
        conflict = next(o for o in output if o['external_id'] == '<conflict@example.invalid>')
        self.assertEqual(conflict['payload']['refusal'], 'message_id_conflicting_content')
        self.assertEqual(read(list(reversed(rows))), output)

    def test_packet_has_bounded_title_body_and_digest_without_raw_duplication(self):
        m = self.module()
        output = m.parse_mail_read({'reachable': True, 'complete': True,
                                    'messages': [row(message(subject='x' * 1000))]}, None)
        described = m.describe(output[0]['payload'])
        self.assertLessEqual(len(described['title']), 256)
        self.assertEqual(described['metadata']['body'], 'Whole message body\n')
        self.assertNotIn('raw_message_base64', described['metadata'])
        self.assertNotIn('raw_message_base64', output[0]['payload'])
        self.assertTrue(output[0]['payload']['source_digest'].startswith('sha256:'))

    def test_packet_projection_keeps_the_original_fixed_field_contract(self):
        m = self.module()
        payload = m.parse_mail_read({'reachable': True, 'complete': True,
                                     'messages': [row(message())]}, None)[0]['payload']
        with self.assertRaises(m.Unreachable):
            m.describe({**payload, 'unexpected': 'not-an-approved-packet-field'})

    def test_serialized_read_and_packet_fit_real_json_doors_or_named_refusal(self):
        m = self.module()
        raw = message(body='\"\\\x01\n' * (400 * 1024))
        batch = {'reachable': True, 'complete': True,
                 'messages': [row(raw), row(message('good'))]}
        output = m.parse_mail_read(batch, None)
        self.assertEqual(len(output), 2)
        self.assertTrue(any(o['payload'].get('refusal') for o in output))
        # Axum's default is2MiB; reserve half for surrounding request fields.
        self.assertLess(len(json.dumps(output).encode()), 1024 * 1024)
        for observation in output:
            described = m.describe(observation['payload'])
            self.assertLess(len(json.dumps({'metadata': described['metadata'],
                                            'title': described['title']}).encode()),
                            1024 * 1024)

    def test_257_timestamp_ties_return_bounded_named_poll_refusal_not_silent_truncation(self):
        m = self.module()
        rows = [row(message(str(i))) for i in range(257)]
        def refusal(values):
            with self.assertRaises(m.Unreachable) as error:
                m.parse_mail_read({'reachable': True, 'complete': True, 'messages': values}, None)
            witness = getattr(error.exception, 'witness', None)
            self.assertIsInstance(witness, dict, 'source must carry the bounded poll-refusal receipt')
            return witness
        witness = refusal(rows)
        self.assertEqual(len(witness['entries']), 64)
        self.assertEqual(witness['truncated'], 193)
        self.assertTrue(all(e['at'] == AT and e['message_key'].startswith('<')
                            and e['size_bytes'] > 0
                            and e['limit'] == {'unit': 'count', 'value': 256, 'observed': 257}
                            and e['reason'] == 'timestamp_tie_bound' for e in witness['entries']))
        self.assertEqual(refusal(list(reversed(rows))), witness)
        self.assertLess(len(json.dumps(witness).encode()), 64 * 2048)

    def test_import_leaves_source_tree_free_of_bytecode(self):
        with tempfile.TemporaryDirectory(prefix='support-mail-import-') as directory:
            source = Path(directory) / 'support_mail.py'
            source.write_bytes(SCRIPT.read_bytes())
            spec = importlib.util.spec_from_file_location('isolated_support_mail', source)
            value = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(value)
            self.assertFalse((source.parent / '__pycache__').exists(),
                             'source-only imports must not leave binary scanner inputs')

    def test_new_mail_from_actual_standin_has_stable_id_and_full_packet_body(self):
        raw = message(body='Full Unicode body: café\nsecond line\n')
        with standin([raw]) as (port, calls):
            batch = mailbox_read(port)
        m = self.module()
        observations = m.parse_mail_read(batch, None)
        self.assertEqual(len(observations), 1)
        observation = observations[0]
        self.assertEqual(observation['external_id'], '<one@example.invalid>')
        self.assertEqual(observation['observed_at'], AT)
        self.assertEqual(observation['payload']['body'], 'Full Unicode body: café\nsecond line\n')
        self.assertNotIn('raw_message_base64', observation['payload'])
        self.assertEqual(observation['payload']['source_digest'], 'sha256:' + hashlib.sha256(raw).hexdigest())
        described = m.describe(observation['payload'])
        self.assertEqual(described['title'], 'Customer question')
        self.assertEqual(described['metadata'],
                         {k: v for k, v in observation['payload'].items()
                          if k != 'raw_message_base64'})
        self.assertIn('EXAMINE', calls)
        self.assertNotIn('SELECT', calls)
        self.assertNotIn('STORE', calls)

    def test_authenticated_empty_is_good_but_polite_signed_out_empty_is_unreachable(self):
        with standin() as (port, calls):
            empty = mailbox_read(port)
        with standin(signed_in=False) as (port, refused_calls):
            signed_out = mailbox_read(port)
        m = self.module()
        self.assertEqual(m.parse_mail_read(empty, None), [])
        with self.assertRaises(m.Unreachable):
            m.parse_mail_read(signed_out, None)
        self.assertIn('CAPABILITY', refused_calls)
        self.assertIn('LOGIN', refused_calls)
        self.assertNotIn('UID', refused_calls)

    def test_wrong_credentials_do_not_become_an_empty_success(self):
        with standin([message()]) as (port, calls):
            batch = mailbox_read(port, 'wrong-synthetic-fixture')
        m = self.module()
        with self.assertRaises(m.Unreachable) as error:
            m.parse_mail_read(batch, None)
        self.assertNotIn(SYNTHETIC_PASSWORD, str(error.exception))
        self.assertNotIn('UID', calls)

    def test_duplicate_message_id_replay_is_stable_and_conflicting_body_is_named(self):
        raw = message()
        with standin([raw, raw]) as (port, _):
            batch = mailbox_read(port)
        m = self.module()
        first = m.parse_mail_read(batch, None)
        self.assertEqual(len(first), 1)
        self.assertEqual(m.parse_mail_read(batch, AT), first)
        self.assertEqual(m.parse_mail_read({'reachable': True, 'complete': True, 'messages': list(reversed(batch['messages']))}, None), first)
        conflict = m.parse_mail_read({'reachable': True, 'complete': True,
                                      'messages': [row(raw), row(message(body='Contradiction'))]}, None)
        self.assertEqual(len(conflict), 1)
        self.assertEqual(conflict[0]['external_id'], '<one@example.invalid>')
        self.assertEqual(conflict[0]['payload']['refusal'], 'message_id_conflicting_content')

    def test_cursor_includes_both_ids_at_boundary_and_ignores_sender_date(self):
        m = self.module()
        batch = {'reachable': True, 'complete': True, 'messages': [row(message('before'), '2026-10-31T20:59:59Z'), row(message('two')), row(message('one'))]}
        output = m.parse_mail_read(batch, AT)
        self.assertEqual([o['external_id'] for o in output], ['<one@example.invalid>', '<two@example.invalid>'])
        self.assertEqual([o['observed_at'] for o in output], [AT, AT])
        self.assertEqual(m.parse_mail_read(batch, '2026-10-31T21:00:01Z'), [])

    def test_unknown_partial_or_malformed_read_refuses_even_when_empty(self):
        m = self.module()
        for batch in ({}, {'reachable': None, 'complete': True, 'messages': []}, {'reachable': 1, 'complete': True, 'messages': []}, {'reachable': True, 'complete': False, 'messages': []}, {'reachable': True, 'complete': True, 'messages': None}):
            with self.subTest(batch=batch), self.assertRaises(m.Unreachable):
                m.parse_mail_read(batch, None)

    def test_fractional_source_times_sort_chronologically_not_as_strings(self):
        m = self.module()
        batch = {'reachable': True, 'complete': True,
                 'messages': [row(message('whole'), AT),
                              row(message('fraction'), '2026-10-31T21:00:00.1Z')]}
        output = m.parse_mail_read(batch, None)
        self.assertEqual([o['external_id'] for o in output],
                         ['<whole@example.invalid>', '<fraction@example.invalid>'])
        self.assertEqual(len(m.parse_mail_read(batch, '2026-10-31T21:00:00.1Z')), 1)

    def test_bad_message_bytes_are_named_but_unknown_receipt_time_refuses_transport(self):
        m = self.module()
        good = message()
        rows = [row(good.replace(b'Message-ID:', b'X-Message-ID:')), row(b'Message-ID: <second@example.invalid>\r\n'+good), row(good.replace(b'<one@example.invalid>', b'not-an-id')), {'received_at': AT, 'raw_message_base64': '%%%bad'}]
        for bad in rows:
            with self.subTest(bad_kind=list(bad)):
                output = m.parse_mail_read({'reachable': True, 'complete': True, 'messages': [bad]}, None)
                self.assertEqual(len(output), 1)
                self.assertTrue(output[0]['payload']['refusal'])
                self.assertEqual(m.parse_mail_read({'reachable': True, 'complete': True,
                                                    'messages': [bad]}, AT), output)
        with self.assertRaises(m.Unreachable):
            m.parse_mail_read({'reachable': True, 'complete': True,
                               'messages': [row(good, '2026-10-31T21:00:00')]}, None)

    def test_byte_refusals_preserve_ids_and_serialized_read_bound(self):
        m = self.module()
        oversized = m.parse_mail_read({'reachable': True, 'complete': True,
                                      'messages': [row(b'x'*(m.MAX_MESSAGE_BYTES+1))]}, None)
        self.assertEqual(len(oversized), 1)
        self.assertEqual(oversized[0]['payload']['refusal'], 'message_byte_bound')
        rows = [row(message(str(i), body='\"\\\n' * 4000)) for i in range(m.MAX_MESSAGES)]
        with self.assertRaises(m.PollRefused) as error:
            m.parse_mail_read({'reachable': True, 'complete': True, 'messages': rows}, None)
        self.assertTrue(all(e['reason'] == 'timestamp_tie_read_byte_bound'
                            for e in error.exception.witness['entries']))
        # A complete transport window beyond its declared row bound remains
        # UNKNOWN; timestamp-tie pagination belongs to the transport contract.
        with self.assertRaises(m.Unreachable):
            m.parse_mail_read({'reachable': True, 'complete': True,
                               'messages': [row(message())] * (m.MAX_MESSAGES+1)}, None)

    def test_deep_mime_is_a_named_message_refusal_not_an_uncaught_recursion_error(self):
        m = self.module()
        raw = b'Content-Type: text/plain\r\n\r\nFull leaf body\r\n'
        for index in range(1100):
            boundary = f'part{index}'.encode()
            raw = (b'Content-Type: multipart/mixed; boundary="'+boundary+b'"\r\n\r\n--'
                   +boundary+b'\r\n'+raw+b'\r\n--'+boundary+b'--\r\n')
        raw = b'Message-ID: <deep@example.invalid>\r\n'+raw
        self.assertLess(len(raw), m.MAX_MESSAGE_BYTES)
        output = m.parse_mail_read({'reachable': True, 'complete': True,
                                    'messages': [row(raw)]}, None)
        self.assertEqual(len(output), 1)
        self.assertEqual(output[0]['external_id'], '<deep@example.invalid>')
        self.assertTrue(output[0]['payload']['refusal'])


if __name__ == '__main__':
    unittest.main(verbosity=2)
