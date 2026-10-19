"""
Compute Morgan (ECFP4) fingerprints for sembio.Compound and write to fp column.

- Keyset pagination on id (requires id index) — no OFFSET
- radius=2, 1024-bit Morgan FP (industry standard) bit-packed to 128 bytes
- Stored as Bytes: base64 of the packed bits (DataType::Bytes convention)
- No vector index during bulk write (OOM). Rebuild one after import if needed.

Usage:
    python import_fp.py [--limit N] [--workers 4] [--resume] [--page 500]
"""
import argparse
import base64
import json
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed

from rdkit import Chem, RDLogger
from rdkit.Chem import rdFingerprintGenerator as fg

RDLogger.DisableLog("rdApp.*")

BASE = "http://127.0.0.1:7912"
STATE_FILE = r"E:\ontodb\fp_import_state.json"
DIM = 1024
GEN = fg.GetMorganGenerator(radius=2, fpSize=DIM)


def q(sql, timeout=180):
    b = json.dumps({"query": sql}).encode()
    r = urllib.request.Request(
        BASE + "/api/query",
        data=b,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(r, timeout=timeout) as resp:
        return json.loads(resp.read())


def fp_b64(smiles):
    """Morgan FP → bit-packed bytes → base64 string (Bytes storage form)."""
    m = Chem.MolFromSmiles(smiles)
    if m is None:
        return None
    fp = GEN.GetFingerprint(m)
    bits = fp.GetOnBits()
    packed = bytearray(DIM // 8)
    for i in bits:
        packed[i // 8] |= 1 << (7 - (i % 8))
    return base64.b64encode(bytes(packed)).decode("ascii")


def esc(s):
    return s.replace("'", "''")


# update_one triage statuses (0-row must NOT be a blanket failure — idempotent
# re-runs and concurrent writers legitimately produce 0-row outcomes):
#   ok        — UPDATE matched >=1 row and wrote fp
#   already   — 0 rows but the row already holds exactly this fp (idempotent hit)
#   missing   — 0 rows and the row does not exist (bad id / deleted)          → error
#   unwritten — 0 rows and the row exists but fp is still NULL                → error
#   conflict  — 0 rows and the row exists with a *different* fp (raced/lost)  → error
STATUS_OK = "ok"
STATUS_ALREADY = "already"
STATUS_MISSING = "missing"
STATUS_UNWRITTEN = "unwritten"
STATUS_CONFLICT = "conflict"


def parse_updated_count(d):
    """Extract N from 'N row(s) updated' in ApiResponse data.message."""
    data = d.get("data")
    msg = ""
    if isinstance(data, dict):
        msg = data.get("message") or ""
    elif isinstance(data, str):
        msg = data
    for token in msg.replace("(", " ").replace(")", " ").split():
        if token.isdigit():
            return int(token)
    return None


def triage_zero_rows(cid, b64):
    """0-row fell through: decide which of the three semantics applies."""
    try:
        d = q(
            f"SELECT fp FROM sembio.Compound WHERE id = '{esc(cid)}' LIMIT 1",
            timeout=60,
        )
    except Exception as e:
        return STATUS_UNWRITTEN, f"triage_read_err:{type(e).__name__}"
    rows = d.get("data") or []
    if not rows:
        return STATUS_MISSING, "row_absent"
    fp = rows[0].get("fp")
    if fp == b64:
        return STATUS_ALREADY, "fp_already_set"
    if fp is None or fp == "":
        return STATUS_UNWRITTEN, "fp_still_null"
    return STATUS_CONFLICT, "fp_differs"


def update_one(cid, b64):
    # WHERE id only: a residual `AND fp IS NULL` used to make the planner drop
    # the id index and seq-scan the whole Compound table per UPDATE, which
    # froze the machine. Idempotency is handled by the page-level skip of rows
    # that already carry fp, plus triage of 0-row outcomes below.
    sql = (
        f"UPDATE sembio.Compound SET fp = '{b64}' "
        f"WHERE id = '{esc(cid)}'"
    )
    try:
        d = q(sql, timeout=180)
        if not d.get("success"):
            return cid, STATUS_UNWRITTEN, f"api_success_false:{d.get('error')}"
        n = parse_updated_count(d)
        if n is None:
            # Message shape drifted — treat as unknown-but-claimed-success,
            # do not silently count as written.
            return cid, STATUS_UNWRITTEN, "unparsed_rowcount"
        if n >= 1:
            return cid, STATUS_OK, f"n={n}"
        status, detail = triage_zero_rows(cid, b64)
        return cid, status, detail
    except Exception as e:
        return cid, STATUS_UNWRITTEN, f"ERR:{type(e).__name__}"


def load_state():
    try:
        with open(STATE_FILE) as f:
            return json.load(f)
    except Exception:
        return {"last_id": "", "done": 0, "bad_smi": 0}


def save_state(st):
    import os

    tmp = STATE_FILE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(st, f)
        f.flush()
        os.fsync(f.fileno())  # crash-safe: never rename unflushed content
    os.replace(tmp, STATE_FILE)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=0, help="stop after N updates (0=all)")
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--page", type=int, default=1500)
    ap.add_argument("--log", default=r"E:\ontodb\fp_import.log")
    args = ap.parse_args()

    st = load_state() if args.resume else {"last_id": "", "done": 0, "bad_smi": 0}
    last_id = st.get("last_id") or ""
    total_done = int(st.get("done") or 0)
    bad_smi = int(st.get("bad_smi") or 0)

    logf = open(args.log, "a", encoding="utf-8")

    def log(msg):
        line = f"[{time.strftime('%H:%M:%S')}] {msg}"
        print(line, flush=True)
        logf.write(line + "\n")
        logf.flush()

    log(f"start dim={DIM} workers={args.workers} page={args.page} resume={args.resume} last_id={last_id!r} done={total_done}")

    t_start = time.time()
    updated = 0
    already = 0
    missing = 0
    unwritten = 0
    conflict = 0
    errors = 0
    skip_has_fp = 0
    skip_no_smi = 0

    while True:
        if args.limit and updated >= args.limit:
            log(f"limit {args.limit} reached")
            break

        # Keyset range scan; ORDER BY id currently hangs (planner does not use id index).
        # id > '' walks the index from the start in lexicographic order.
        t0 = time.time()
        rows = q(
            f"SELECT id, smiles, fp FROM sembio.Compound WHERE id > '{esc(last_id)}' LIMIT {args.page}",
            timeout=180,
        ).get("data") or []
        if not rows:
            log("no more rows")
            break

        # Pages are not guaranteed ordered; checkpoint at the max handled id.
        page_ids = [r.get("id") for r in rows if r.get("id")]
        handled_ids = []  # ids fully done this page (written / already / skipped)

        jobs = []
        skipped = 0
        skip_has_fp_page = 0
        for r in rows:
            cid = r.get("id")
            smi = r.get("smiles")
            if not cid:
                continue
            # Skip rows that already have a fingerprint.
            if r.get("fp"):
                skipped += 1
                skip_has_fp += 1
                skip_has_fp_page += 1
                handled_ids.append(cid)
                continue
            if not smi:
                skipped += 1
                skip_no_smi += 1
                handled_ids.append(cid)
                continue
            b64 = fp_b64(smi)
            if b64 is None:
                bad_smi += 1
                skipped += 1
                handled_ids.append(cid)
                continue
            jobs.append((cid, b64))

        ok = 0
        page_already = 0
        if jobs:
            with ThreadPoolExecutor(max_workers=args.workers) as ex:
                futs = [ex.submit(update_one, cid, b64) for cid, b64 in jobs]
                for f in as_completed(futs):
                    cid, status, detail = f.result()
                    if status == STATUS_OK:
                        ok += 1
                        handled_ids.append(cid)
                    elif status == STATUS_ALREADY:
                        # Idempotent hit: row already carries this fp. Not a failure.
                        already += 1
                        page_already += 1
                        handled_ids.append(cid)
                    elif status == STATUS_MISSING:
                        missing += 1
                        errors += 1
                        # Row is gone — nothing to write; don't block the keyset.
                        handled_ids.append(cid)
                        if errors <= 5:
                            log(f"missing {cid}: {detail}")
                    elif status == STATUS_CONFLICT:
                        conflict += 1
                        errors += 1
                        if errors <= 5:
                            log(f"conflict {cid}: {detail}")
                    else:  # STATUS_UNWRITTEN / unexpected
                        unwritten += 1
                        errors += 1
                        if errors <= 5:
                            log(f"unwritten {cid}: {detail}")

        updated += ok
        total_done += ok
        # Advance the keyset only over the contiguous handled prefix in id
        # order. An all-fail page (timeout/reset) keeps last_id unchanged so
        # --resume retries the same window; a gap in the middle stops the
        # advance at the gap so the failed row is not skipped forever.
        handled_set = set(handled_ids)
        for cid in sorted(page_ids):
            if cid in handled_set:
                last_id = cid
            else:
                break
        dt = time.time() - t0
        rate = total_done / max(time.time() - t_start, 0.001)
        log(
            f"page rows={len(rows)} jobs={len(jobs)} ok={ok} already={page_already} "
            f"skip={skipped} "
            f"(has_fp={skip_has_fp_page} no_smi={skip_no_smi} bad_smi={bad_smi}) "
            f"err={errors} (missing={missing} unwritten={unwritten} conflict={conflict}) "
            f"done={total_done} last_id={last_id} "
            f"page_time={dt:.1f}s rate={rate:.1f}/s"
        )
        save_state({"last_id": last_id, "done": total_done, "bad_smi": bad_smi})

    log(
        f"DONE updated={updated} already={already} total_done={total_done} "
        f"bad_smi={bad_smi} missing={missing} unwritten={unwritten} "
        f"conflict={conflict} errors={errors}"
    )
    logf.close()


if __name__ == "__main__":
    main()
