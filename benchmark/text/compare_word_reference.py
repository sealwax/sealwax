"""Compare Sealwax words to an independently generated JSON reference, offline.

    python -I compare_word_reference.py reference.json directory_of_pdfs

The reference is keyed by SHA-256 of the PDF bytes. Each value is an ordered list
of pages [metadata, words]. Each word is [digest, x0, y0, x1, y1, block, line,
word], in unrotated points from the top left of the crop box. The digest is the
first 12 hex characters of SHA-1 over the UTF-8 word. Include annotations on
both sides. On each nonempty page, also search for a middle word and, if present,
the first pair straddling two lines of a block. Check whether a hit covers those
reference boxes. These probes measure highlight placement at known locations.
Only aggregate counts are printed.
"""

import argparse
import hashlib
import json
from collections import Counter
from difflib import SequenceMatcher
from pathlib import Path

import sealwax


def reference_box(box, geometry):
    x0, y0, x1, y1 = box
    left, _, _, top = geometry.crop_box
    unit = geometry.user_unit
    return ((x0 - left) * unit, (top - y1) * unit, (x1 - left) * unit, (top - y0) * unit)


def near(box, reference):
    return all(abs(a - b) <= 1 for a, b in zip(box, reference[1:5]))


def compare_page(counts, reference, words, geometry):
    expected = [row[0] for row in reference]
    found = [hashlib.sha1(w["text"].encode()).hexdigest()[:12] for w in words]
    counts["pages"] += 1
    counts["identical_word_sequences"] += expected == found
    counts["reference_words"] += len(expected)
    counts["extracted_words"] += len(found)
    for start, other, length in SequenceMatcher(None, expected, found, autojunk=False).get_matching_blocks():
        counts["aligned_words"] += length
        # A block number can differ for an entire page after one extra block.
        # Also score the actual breaks between adjacent words aligned on both sides.
        for offset in range(1, length):
            prev, cur = reference[start + offset - 1:start + offset + 1]
            left, right = words[other + offset - 1:other + offset + 1]
            block = left["block"] != right["block"]
            for kind, actual, wanted in [
                ("block", block, prev[5] != cur[5]),
                ("line", block or left["line"] != right["line"], prev[5:7] != cur[5:7]),
            ]:
                counts["extracted_" + kind + "_breaks"] += actual
                counts["reference_" + kind + "_breaks"] += wanted
                counts["matched_" + kind + "_breaks"] += actual and wanted
        for row, word in zip(reference[start:start + length], words[other:other + length]):
            counts["boxes_within_one_point"] += near(reference_box(word["bbox"], geometry), row)
            counts["equal_block_indices"] += row[5] == word["block"]
            counts["equal_line_indices"] += row[6] == word["line"]
            counts["equal_word_indices"] += row[7] == word["word"]
    return dict(zip(found, (word["text"] for word in words)))


def compare_hit(counts, kind, rows, labels, doc, page, geometry):
    counts[kind + "_probes"] += 1
    if any(row[0] not in labels for row in rows):
        counts[kind + "_unavailable_labels"] += 1
        return
    query = " ".join(labels[row[0]] for row in rows)
    if len(rows) == 2 and labels[rows[0][0]].endswith("-"):
        # The final painted hyphen's width is unknown in a word-only reference.
        counts[kind + "_hyphenated"] += 1
        return
    for hit in doc.search(page, query, annotations=True):
        if len(hit) != len(rows):
            continue
        boxes = []
        for quad in hit:
            xs, ys = zip(*quad)
            boxes.append(reference_box((min(xs), min(ys), max(xs), max(ys)), geometry))
        if all(near(box, row) for box, row in zip(boxes, rows)):
            counts[kind + "_within_one_point"] += 1
            return


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reference", type=Path)
    parser.add_argument("corpus", type=Path)
    args = parser.parse_args()
    reference = json.loads(args.reference.read_bytes())
    counts = Counter(reference_documents=len(reference), documents=0, missing_documents=0, page_count_mismatches=0)
    for path in sorted(args.corpus.rglob("*.pdf")):
        data = path.read_bytes()
        pages = reference.pop(hashlib.sha256(data).hexdigest(), None)
        if pages is None:
            continue
        doc = sealwax.Document.from_bytes(data)
        counts["documents"] += 1
        counts["page_count_mismatches"] += doc.page_count != len(pages)
        for i, ((_, expected), geometry) in enumerate(zip(pages, doc.geometry())):
            labels = compare_page(counts, expected, doc.words(i, annotations=True), geometry)
            if expected:
                middle = expected[len(expected) // 2]
                compare_hit(counts, "word_search", [middle], labels, doc, i, geometry)
            for before, after in zip(expected, expected[1:]):
                if before[5] == after[5] and before[6] != after[6]:
                    compare_hit(counts, "multiline_search", [before, after], labels, doc, i, geometry)
                    break
    counts["missing_documents"] = len(reference)
    print(json.dumps(counts, sort_keys=True))
    return int(bool(counts["missing_documents"] or counts["page_count_mismatches"]))


if __name__ == "__main__":
    raise SystemExit(main())
