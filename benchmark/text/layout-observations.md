# Page text observations

These examples were measured with PyMuPDF 2.1 using its public `get_text("words")`
and `search_for(query, quads=True)` methods. All pages were synthetic, 500 × 500 pt.
They used standard Courier at 20 pt and an initial text position of (50, 250) in
PDF user space unless specified otherwise.

| Input | Observed output |
|---|---|
| `[(ab) -100 (cd)] TJ` | One word, `abcd`. |
| The same array with −200, −300, −500 or −750 | Two words on one line, `ab` and `cd`. |
| The same array with −1000 or −2000 | Two words on separate lines of one block. |
| `(ab) Tj 0 -24 Td (cd) Tj` | Two lines of one block. A 30 pt drop also kept one block. Drops of 34 and 42 pt produced two blocks. |
| `(ab) Tj 0 -50 Td (cd) Tj` and the same with a 60 pt rise | Two blocks, with `ab` first in both cases. |
| Text in render mode 3 | The invisible word `ghost` was extracted and searchable. |
| A single `B` drawn twice at the same position | One extracted word and one search hit. |
| `/ActualText (lake shore)` covering `xxxxx`, followed by a period | Words `lake` and `shore.`. Searching for `xxxxx` returned no hits. |
| `first` followed by `second` on the next line | Searching for `st se` or `FIRST SECOND` returned two quads. Searching for `stse` returned none. |
| `multi-` at the end of a line, followed by `line` | Searching for `multiline` returned two quads. The queries `multi-line` and `multi line` returned none. |
| `Red-fox` on one line | Searching for `red-FOX` returned one quad. |
| `abc` under the text matrix `.8 .6 -.6 .8 100 200` | Searching for `b` returned one quad. Its upper edge advanced 9.6 pt horizontally and 7.2 pt vertically in PDF user space. |

Sealwax reports geometry in unrotated PDF user space, with y increasing upwards.
Its search API groups the quads belonging to each occurrence. The corpus comparison
uses an independently recorded word reference and reports aggregate counts.

## Development corpus

The offline comparison covered 98 documents and all 458 reference pages, with
annotation appearances enabled. The complete word sequence agreed on 357 pages.

| Measurement | Count |
|---|---:|
| Reference words matched in order | 204,906 / 205,479 |
| Aligned word boxes with all four coordinates within 1 pt | 204,739 / 204,906 |
| Reference line breaks recovered between aligned neighbours | 29,476 / 29,765 |
| Reference block breaks recovered between aligned neighbours | 11,625 / 13,221 |
| Predicted block breaks confirmed between aligned neighbours | 11,625 / 12,178 |
| Aligned words with identical block / line / word indices | 38,506 / 150,372 / 202,738 out of 204,906 |

Sealwax numbers text blocks consecutively from zero. Reference numbering can skip
indices, and a different block boundary can shift the indices of later words.

The search probes checked the known location of a word on 446 nonempty pages
and a first cross-line pair on 408 eligible pages. A hit covered the reference
boxes within 1 pt in 425 word probes and 402 cross-line probes. Five single words
and one pair lacked a matching text digest on the page. Four other pairs ended in
a hyphen whose individual geometry the word reference does not record. The
reference for these probes contains word boxes. Full search-result counts and
oriented corner order are covered separately by synthetic tests.
