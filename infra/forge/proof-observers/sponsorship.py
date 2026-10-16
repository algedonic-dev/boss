"""Sponsorship admission is an event witness, never a consent-copy verdict."""


def observe(evidence, rows, cut, now, _contract):
    found = []
    for row in rows:
        opened = evidence.instant(row["opened_at"])
        if opened > now: raise evidence.Unknown(row["id"] + ": server admission is in the future")
        if opened > cut: found.append({"id": row["id"], "opened_at": row["opened_at"]})
    return evidence.observation(found, cut, now)
