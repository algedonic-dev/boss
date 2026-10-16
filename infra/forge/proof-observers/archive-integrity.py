"""Remote archive integrity is narrower than restore or backup creation."""
import re


def observe(evidence, rows, cut, now, contract):
    found = []
    for row in rows:
        step = evidence.execution(row, contract, cut, now)
        if step is None: continue
        metadata = step["metadata"]
        if metadata.get("exit_code") != "0" or metadata.get("effect") != "check-gcs-backup: READ — remote archive integrity verified":
            continue
        output = metadata.get("output")
        if not isinstance(output, str): raise evidence.Unknown(row["id"] + ": complete native output unavailable")
        receipts = [line.removeprefix("receipt: ") for line in output.splitlines() if line.startswith("receipt: ")]
        if len(receipts) != 1: raise evidence.Unknown(row["id"] + ": native receipt missing or ambiguous")
        try: body = evidence.document(receipts[0])
        except evidence.Unknown as error: raise evidence.Unknown(row["id"] + ": " + str(error)) from error
        if not isinstance(body, dict): raise evidence.Unknown(row["id"] + ": archive receipt is not an object")
        valid = (body.get("claim") == "remote-archive-integrity" and body.get("result") == "ok"
                 and body.get("stage") == "verified" and body.get("gzip_test") is True
                 and body.get("completion_trailer") is True
                 and isinstance(body.get("generation"), str) and re.fullmatch(r"[1-9][0-9]*", body["generation"])
                 and isinstance(body.get("object"), str) and re.fullmatch(r"boss-[0-9]{8}-[0-9]{6}\.sql\.gz", body["object"])
                 and isinstance(body.get("sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", body["sha256"])
                 and all(type(body.get(key)) is int and body[key] > 0 for key in ("object_bytes", "downloaded_bytes", "listed_pages", "matching_dumps"))
                 and body["downloaded_bytes"] == body["object_bytes"])
        if not valid: raise evidence.Unknown(row["id"] + ": remote integrity receipt does not establish its declared claim")
        checked = evidence.instant(body.get("checked_at"))
        if not evidence.instant(row["opened_at"]) <= checked <= evidence.instant(step["completed_at"]):
            raise evidence.Unknown(row["id"] + ": integrity timestamp outside server execution bounds")
        found.append({"id": row["id"], "receipt": body})
    return evidence.observation(found, cut, now)
