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

## Baseline offsets, overlap and writing direction

During review, both engines read the same synthetic PDFs. The observations below
characterize the Sealwax settings after implementation. For each page, the text
stream was `BT /F 20 Tf 50 250 Td (ab) Tj c s -s c x y Tm (cd) Tj ET`, with
`/F` set to Courier. The first word ends at (74, 250). In the translation tests
`c = 1` and `s = 0`. In the rotation tests `(x, y) = (74, 250)`, and `c` and
`s` are the cosine and sine of the angle. Word, line and block membership came
from the indices returned by each engine's public word API.

| Vary one quantity | Sealwax | PyMuPDF 2.1 |
|---|---|---|
| Perpendicular displacement from the end of `ab`, in either direction | 8 pt kept one word. 8.1 and 12 pt produced two lines in one block. | 8, 8.1 and 12 pt kept one word. At 18 pt the text occupied two blocks. |
| Backward displacement on the same baseline | 10 pt kept one word. 10.1, 15 and 16 pt produced two lines. | 10, 10.1 and 15 pt kept one word. 16 pt produced two lines. |
| Rotation of `cd` about the end of `ab` | 2.5°, 3° and 5.7° kept one word. 5.8° produced two blocks. | 2.5° kept one word. 3°, 5.7° and 5.8° produced two blocks. |

At 20 pt, the Sealwax tolerances of 0.4 em across and −0.5 em along the baseline
correspond to 8 pt and −10 pt. The direction test compares unit vectors. A dot
product of 0.995 corresponds to about 5.73°. These are Sealwax choices, and the
fixtures identify where their output differs from the reference.

## Indented paragraphs and column restarts

The first run now draws `abcd`. Position the second horizontal run at
`(50 + indent × H, 250 + rise × H)`, where `H = 24.98 pt` is Courier's
measured line height at 20 pt. Positive rise moves up the page. Each of the
following measurements produced two lines.

| Rise / H | Indent / H, in order | Sealwax block counts | PyMuPDF 2.1 block counts |
|---:|---|---|---|
| +0.59 | 0.09, 0.11 | 1, 1 | 1, 1 |
| +0.61 | 0.09, 0.11 | 1, 2 | 1, 1 |
| +1.00 | 0.09, 0.11 | 1, 2 | 2, 2 |
| −0.59 | 0.49, 0.51 | 1, 1 | 1, 1 |
| −0.61 | 0.49, 0.51 | 1, 2 | 1, 1 |
| −1.00 | 0.49, 0.51 | 1, 2 | 2, 2 |

The original calibration used pairs of adjacent words aligned to the frozen
development reference. Before the paragraph rule, 3,398 of 19,660 candidate
line breaks also needed a block break. For these horizontal measurements, the
height proxy was the taller word box of the pair, the vertical displacement was
between their centres, and indentation was measured from the previous line's
first word. An upward move over 0.6 heights with indentation over 0.1 heights
selected 860 pairs, of which 818 were reference block breaks. A downward move
over 0.6 heights with indentation over 0.4 heights selected 1,487 pairs, of
which 1,018 were reference block breaks. Applying the shipped 0.5 indentation
cutoff to those recorded pairs during review selected 1,398, including 932
reference block breaks. The shipped rule measures displacements
between line starts in the writing direction and uses the preceding line's
first glyph height, at least one em.

### Sensitivity on the development corpus

Starting from the settings above, each run varied one threshold in
`layout.rs` and repeated the word and break comparisons from
`compare_word_reference.py` on the same 98 PDFs with the same frozen
reference. The baseline matched 204,906 words in order and
had 296 line-break errors and 2,149 block-break errors. Each error count adds
missing and extra breaks between adjacent words aligned on both sides. The
two deltas in each cell correspond to the two alternative settings in order.

| Threshold | Baseline | Alternatives | Δ matched words | Δ line-break errors | Δ block-break errors |
|---|---:|---|---|---|---|
| Perpendicular offset (em) | 0.4 | 0.2, 0.6 | −363, +6 | +45, −7 | +21, −1 |
| Backward offset (em) | −0.5 | −0.3, −0.75 | 0, +14 | −3, 0 | 0, 0 |
| Direction dot product | 0.995 | 0.99, 0.999 | 0, 0 | 0, 0 | 0, 0 |
| Vertical displacement (line heights) | 0.6 | 0.5, 0.7 | 0, 0 | 0, 0 | +43, +21 |
| Indent on an upward move (line heights) | 0.1 | 0, 0.2 | 0, 0 | 0, 0 | +3, +2 |
| Indent on a downward move (line heights) | 0.5 | 0.4, 0.6 | 0, 0 | 0, 0 | −83, −4 |

The direction sweep leaves all three corpus counts tied. On the rotation
fixture, a dot-product cutoff of 0.999 splits the text at 3°, whereas 0.99
still gives one word at 6°. On this corpus a downward indent of 0.4
line heights gives fewer block-break errors, and a perpendicular
tolerance of 0.6 em improves all three measures slightly. These comparisons
describe sensitivity on the calibration set. Generalization to other documents
would require separate validation.

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
