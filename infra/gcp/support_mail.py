"""Pure mail-source decoding (45c7, A215526 car2a).

The future fixed boss-gcp ops reader supplies a complete, bounded read with
authenticated reachability separate from its message count. This module opens
no connection or credential file and schedules nothing. Its output uses the
existing SensorSource Observation shape; SensorPoll owns durable dedup/opening.
"""
import base64
import binascii
import copy
import datetime
import hashlib
import json
import itertools
from email.errors import UndecodableBytesDefect
from email import policy
from email.parser import BytesParser
import re

MAX_MESSAGES = 256
MAX_MESSAGE_BYTES = 1024 * 1024
# Bound serialized body/headers, not a second base64 copy of the mailbox.
# Any accepted singleton fits a read, with room below the fixed2MiB door.
MAX_OBSERVATION_JSON_BYTES = 1024 * 1024
MAX_READ_JSON_BYTES = 1536 * 1024
_TIMESTAMP = re.compile(r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?:Z|[+-]\d{2}:\d{2})')
_MESSAGE_ID = re.compile(r'<[^<>\s\x00-\x1f\x7f]+@[^<>\s\x00-\x1f\x7f]+>')


class Unreachable(ValueError):
    """Unreadable/unknown is never an empty successful source read."""


class PollRefused(Unreachable):
    """A bounded source receipt; no reading or cursor advance is certified."""

    def __init__(self, witness):
        super().__init__('mail source timestamp tie exceeds bounded read')
        self.witness = witness


class MessageRefused(ValueError):
    """A permanent row fault is recorded, so it cannot poison the cursor."""


def _json_size(value):
    return len(json.dumps(value, ensure_ascii=True, separators=(',', ':')).encode('ascii'))


def _identifier(message):
    ids = message.get_all('Message-ID', [])
    if len(ids) != 1:
        raise MessageRefused('message_id_unavailable')
    identifier = str(ids[0]).strip()
    if ids[0].defects or len(identifier) > 512 or not identifier.isascii() or not _MESSAGE_ID.fullmatch(identifier):
        raise MessageRefused('message_id_unavailable')
    return identifier


def _timestamp(value):
    if not isinstance(value, str) or not _TIMESTAMP.fullmatch(value):
        raise Unreachable('mail source timestamp unavailable')
    try:
        return datetime.datetime.fromisoformat(value.replace('Z', '+00:00')).astimezone(datetime.timezone.utc)
    except ValueError:
        raise Unreachable('mail source timestamp unavailable') from None


def _addresses(message, field):
    result = []
    for header in message.get_all(field, []):
        if '\ufffd' in str(header) or any(not isinstance(d, UndecodableBytesDefect) for d in header.defects):
            raise MessageRefused('address_unavailable')
        result.extend(address.addr_spec for address in header.addresses)
    return result


def _payload(raw):
    try:
        message = BytesParser(policy=policy.default).parsebytes(raw)
        identifier = _identifier(message)
        subjects = message.get_all('Subject', [])
        if len(subjects) > 1:
            raise MessageRefused('subject_ambiguous')
        subject = str(subjects[0]) if subjects else '(no subject)'
        if '\ufffd' in subject or any(ord(char) < 32 and char != '\t' for char in subject):
            raise MessageRefused('subject_unavailable')
        part = message.get_body(preferencelist=('plain', 'html'))
        body = ''
        if part is not None:
            content = part.get_payload(decode=True)
            if not isinstance(content, bytes):
                raise MessageRefused('body_unavailable')
            body = content.decode(part.get_content_charset() or 'utf-8', errors='strict')
        if '\x00' in body:
            raise MessageRefused('body_contains_nul')
        if any(part.defects for part in message.walk()):
            raise MessageRefused('encoding_unavailable')
        return {'message_id': identifier, 'subject': subject, 'body': body,
                'from': _addresses(message, 'From'), 'to': _addresses(message, 'To'),
                'cc': _addresses(message, 'Cc'),
                'source_digest': 'sha256:' + hashlib.sha256(raw).hexdigest()}
    except MessageRefused:
        raise
    except (ValueError, TypeError, AttributeError, LookupError, UnicodeError, RecursionError):
        # A malformed source response may contain body/credential-shaped data.
        # Only fixed-purpose reasons cross into sensor_unreadable diagnostics.
        raise MessageRefused('message_unavailable') from None


def _refusal(identifier, reason, digest):
    return {'message_id': identifier, 'subject': 'Unreadable support message',
            'body': '', 'from': [], 'to': [], 'cc': [],
            'refusal': reason, 'source_digest': digest}


def _row_details(encoded):
    if not isinstance(encoded, str):
        raise Unreachable('mail source row bytes unavailable')
    encoded_digest = hashlib.sha256(encoded.encode('utf-8', errors='surrogatepass')).hexdigest()
    digest = 'base64-sha256:' + encoded_digest
    identifier = 'sha256:' + encoded_digest
    if len(encoded) > ((MAX_MESSAGE_BYTES + 2) // 3) * 4:
        return None, identifier, digest, 'message_byte_bound', None
    try:
        raw = base64.b64decode(encoded, validate=True)
    except (ValueError, binascii.Error):
        return None, identifier, digest, 'message_bytes_unavailable', None
    digest = 'sha256:' + hashlib.sha256(raw).hexdigest()
    identifier = digest
    try:
        identifier = _identifier(BytesParser(policy=policy.default).parsebytes(raw, headersonly=True))
    except (MessageRefused, ValueError, TypeError, AttributeError, UnicodeError, RecursionError):
        pass
    if not raw or len(raw) > MAX_MESSAGE_BYTES:
        return None, identifier, digest, 'message_byte_bound', len(raw)
    return raw, identifier, digest, None, len(raw)


def _row_payload(encoded):
    raw, identifier, digest, reason, _ = _row_details(encoded)
    if reason is not None:
        return _refusal(identifier, reason, digest)
    try:
        return _payload(raw)
    except MessageRefused as error:
        return _refusal(identifier, str(error), digest)


def parse_mail_read(batch, since, read_context=None):
    """Record each bounded message or named refusal; transport faults refuse.

    `received_at` is the transport's mailbox receipt/internal timestamp, not a
    sender's Date header. `since` is inclusive. A timestamp-only cursor cannot
    certify historical backfill; transport coverage remains a2b obligation.
    """
    if not isinstance(batch, dict) or batch.get('reachable') is not True:
        raise Unreachable('mail source unreachable')
    if batch.get('complete') is not True:
        raise Unreachable('mail source read incomplete')
    rows = batch.get('messages')
    if not isinstance(rows, list):
        raise Unreachable('mail source message bound exceeded or unknown')
    cutoff = _timestamp(since) if since is not None else None
    witnessed = set()
    if read_context is not None:
        if (not isinstance(read_context, dict) or set(read_context) != {'witnessed'}
                or not isinstance(read_context['witnessed'], list)
                or len(read_context['witnessed']) > 64):
            raise Unreachable('mail source read context invalid')
        for witness in read_context['witnessed']:
            if (not isinstance(witness, dict) or set(witness) != {'message_key', 'at'}
                    or not isinstance(witness['message_key'], str)
                    or not 0 < len(witness['message_key']) <= 512
                    or any(ord(c) < 32 or ord(c) == 127 for c in witness['message_key'])
                    or cutoff is None or _timestamp(witness['at']) != cutoff):
                raise Unreachable('mail source read context invalid')
            key = (cutoff, witness['message_key'])
            if key in witnessed:
                raise Unreachable('mail source read context has duplicate keys')
            witnessed.add(key)
    prepared = []
    for row in rows:
        if not isinstance(row, dict):
            raise Unreachable('mail source row unavailable')
        at = _timestamp(row.get('received_at'))
        if cutoff is not None and at < cutoff:
            continue
        # Header identity orders the page; do not retain/decode MIME bodies
        # for later pages. The caller supplied this complete bounded batch.
        _, identifier, _, _, size = _row_details(row.get('raw_message_base64'))
        if (at, identifier) in witnessed:
            continue
        prepared.append((at, identifier, row, size))
    prepared.sort(key=lambda value: (value[0], value[1], value[2]['raw_message_base64']))
    if read_context is not None:
        for at, cohort in itertools.groupby(prepared, key=lambda row: row[0]):
            unique = {}
            for candidate in cohort:
                unique.setdefault(candidate[1], candidate)
            count = len(unique) + (len(witnessed) if at == cutoff else 0)
            if count > 64:
                entries = []
                for _, identifier, row, size in list(unique.values())[:64]:
                    if size is None:
                        raise Unreachable('mail source witness overflow measurement unavailable')
                    entries.append({'message_key': identifier, 'at': row['received_at'],
                                    'size_bytes': size,
                                    'limit': {'unit': 'count', 'value': 64, 'observed': count},
                                    'reason': 'cursor_witness_bound'})
                raise PollRefused({'entries': entries, 'truncated': max(0, len(unique)-len(entries))})
    if len(prepared) > MAX_MESSAGES and prepared[0][0] == prepared[-1][0]:
        # An inclusive timestamp cursor cannot page this complete tie safely.
        # Keep bounded names and the measured COUNT, never an unrelated byte
        # ceiling. Real host transport remains the separate2b obligation.
        entries = {}
        for instant, key, _, size in prepared:
            if size is None:
                raise Unreachable('mail source refused cohort raw size unavailable')
            at = instant.isoformat().replace('+00:00', 'Z')
            identity = (at, key)
            if identity in entries or len(entries) == 64:
                continue
            entries[identity] = {'message_key': key, 'size_bytes': size,
                                 'limit': {'unit': 'count', 'value': MAX_MESSAGES,
                                           'observed': len(prepared)},
                                 'reason': 'timestamp_tie_bound', 'at': at}
        ordered = [entries[key] for key in sorted(entries)]
        raise PollRefused({'entries': ordered,
                           'truncated': len(prepared) - len(ordered)})
    observations = {}
    contents = {}
    for at, _, row, _ in prepared[:MAX_MESSAGES]:
        payload = _row_payload(row.get('raw_message_base64'))
        identifier = payload['message_id']
        semantic = json.dumps({k: v for k, v in payload.items()
                               if k not in ('raw_message_base64', 'source_digest')
                               or (k == 'source_digest' and 'refusal' in payload)},
                              sort_keys=True, separators=(',', ':'))
        contents.setdefault(identifier, set()).add(semantic)
        observation = {'external_id': identifier,
                       'observed_at': at.isoformat().replace('+00:00', 'Z'),
                       'payload': payload}
        if identifier in observations:
            previous = observations[identifier]
            # Two aliases can add different Received headers to the SAME id.
            # Choose earliest receipt, then canonical bytes, independently of
            # collection order. The durable reading remains insert-if-absent.
            if (at, json.dumps(payload, sort_keys=True)) < (_timestamp(previous['observed_at']), json.dumps(previous['payload'], sort_keys=True)):
                observations[identifier] = observation
        else:
            observations[identifier] = observation
    for identifier, values in contents.items():
        if len(values) > 1:
            # A trace-only redelivery is equal above; genuinely different
            # content is still refused, but only this Message-ID is affected.
            digest = 'sha256:' + hashlib.sha256(json.dumps(sorted(values)).encode()).hexdigest()
            observation = observations[identifier]
            observations[identifier] = {**observation,
                                        'payload': _refusal(identifier, 'message_id_conflicting_content', digest)}
    ordered = sorted((row for row in observations.values()
                   if cutoff is None or _timestamp(row['observed_at']) >= cutoff),
                  key=lambda row: (_timestamp(row['observed_at']), row['external_id']))
    output = []
    used = 2
    if read_context is not None:
        for observation in ordered:
            if _json_size(observation) > MAX_OBSERVATION_JSON_BYTES:
                observation = {**observation, 'payload': _refusal(observation['external_id'],
                                'observation_json_bound', observation['payload']['source_digest'])}
            size = _json_size(observation)
            if used + size + (1 if output else 0) > MAX_READ_JSON_BYTES:
                break
            used += size + (1 if output else 0)
            output.append(observation)
        return output
    for _, cohort in itertools.groupby(ordered, key=lambda o: _timestamp(o['observed_at'])):
        group = []
        for observation in cohort:
            if _json_size(observation) > MAX_OBSERVATION_JSON_BYTES:
                observation = {**observation, 'payload': _refusal(observation['external_id'],
                                'observation_json_bound', observation['payload']['source_digest'])}
            group.append(observation)
        group_size = _json_size(group)
        if group_size > MAX_READ_JSON_BYTES:
            # A205615 source-side conditional: the caller never sees keys
            # omitted here. Refuse this poll non-permanently; no message is
            # processed, and actual paging/progress remains owed, not proven.
            entries = [{'message_key': o['external_id'], 'at': o['observed_at'],
                        'size_bytes': _json_size(o),
                        'limit': {'unit': 'bytes', 'value': MAX_READ_JSON_BYTES,
                                  'observed': group_size},
                        'reason': 'timestamp_tie_read_byte_bound'} for o in group[:64]]
            raise PollRefused({'entries': entries, 'truncated': len(group)-len(entries)})
        if used + group_size - 2 + (1 if output else 0) > MAX_READ_JSON_BYTES:
            # Defer a WHOLE later timestamp. Its first inclusive reread can
            # return the whole tie, unlike a repeatedly acknowledged prefix.
            break
        used += group_size - 2 + (1 if output else 0)
        output.extend(group)
    return output


def describe(payload):
    """The body stays on the packet; BOSS inbox messages carry its pointer."""
    keys = {'message_id', 'subject', 'body', 'from', 'to', 'cc'}
    shapes = (keys | {'source_digest'}, keys | {'refusal', 'source_digest'})
    if not isinstance(payload, dict) or set(payload) not in shapes or not isinstance(payload['subject'], str):
        raise Unreachable('mail source packet description unavailable')
    metadata = {key: copy.deepcopy(value) for key, value in payload.items() if key != 'raw_message_base64'}
    return {'title': payload['subject'][:256], 'metadata': metadata}
