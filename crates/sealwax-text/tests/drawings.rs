//! The lines and rectangles a page paints, after the current transformation matrix.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, OpenOptions};
use sealwax_text::drawings;

fn page(content: &str) -> Document {
    build(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
         /Resources << /XObject << /Fm 5 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /XObject /Subtype /Form /BBox [0 0 600 800] /Length 17 >>\nstream\n0 0 m 5 5 l 9 9 l S\nendstream"
            .to_owned(),
    ])
}

fn build(objects: &[String]) -> Document {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Document::open(out, &OpenOptions::default()).unwrap()
}

fn close(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
}

#[test]
fn lines_and_rectangles_land_where_the_turned_matrix_puts_them() {
    // A quarter turn and a shift: (x, y) lands at (300 - y, x).
    let found = drawings(
        &page("q 0 1 -1 0 300 0 cm 10 20 m 110 20 l S 50 50 100 30 re f Q"),
        0,
        false,
    )
    .unwrap();
    assert_eq!(found.lines.len(), 1);
    assert!(
        close(&found.lines[0], &[280.0, 10.0, 280.0, 110.0]),
        "{found:?}"
    );
    assert_eq!(found.rectangles.len(), 1);
    assert!(
        close(&found.rectangles[0], &[220.0, 50.0, 250.0, 150.0]),
        "{found:?}"
    );
    assert!(found.quads.is_empty());
}

#[test]
fn a_rectangle_turned_off_the_axes_is_a_quad() {
    // 30 degrees.
    let found = drawings(
        &page("0.866025 0.5 -0.5 0.866025 100 100 cm 0 0 10 20 re S"),
        0,
        false,
    )
    .unwrap();
    assert!(found.rectangles.is_empty());
    assert_eq!(found.quads.len(), 1);
    let [first, second, ..] = found.quads[0];
    assert!(close(&first, &[100.0, 100.0]));
    assert!(close(&second, &[108.66025, 105.0]), "{:?}", found.quads[0]);
}

#[test]
fn what_is_only_clipped_curved_or_painted_twice_is_counted_as_drawn() {
    let found = drawings(
        &page(
            "0 0 10 10 re W n \
             0 0 m 10 10 20 0 30 30 c S \
             5 5 m 15 5 l 15 15 l h S \
             40 40 20 20 re B \
             /Fm Do",
        ),
        0,
        false,
    )
    .unwrap();
    // The clip draws nothing, the curve has no straight side, the triangle closes with a third
    // side, the rectangle filled and stroked counts once, the form's two segments are there.
    assert_eq!(found.rectangles, [[40.0, 40.0, 60.0, 60.0]]);
    assert_eq!(
        found.lines,
        [
            [5.0, 5.0, 15.0, 5.0],
            [15.0, 5.0, 15.0, 15.0],
            [15.0, 15.0, 5.0, 5.0],
            [0.0, 0.0, 5.0, 5.0],
            [5.0, 5.0, 9.0, 9.0]
        ]
    );
}

#[test]
fn a_closed_box_of_four_corners_is_a_rectangle_wherever_it_is() {
    // Alone, among other subpaths of its path, from any corner, closed by `h` or (stroked) by a
    // last side back to its start; two `re` in one path.
    for content in [
        "10 10 m 50 10 l 50 40 l 10 40 l h S",
        "10 10 m 50 10 l 50 40 l 10 40 l h 100 100 m 120 100 l 120 130 l 100 130 l h f",
        "50 40 m 50 10 l 10 10 l 10 40 l s",
        "10 10 m 50 10 l 50 40 l 10 40 l 10 10 l 100 100 m 100 130 l 120 130 l 120 100 l h S",
        "10 10 40 30 re 100 100 20 30 re f",
    ] {
        let found = drawings(&page(content), 0, false).unwrap();
        assert!(found.lines.is_empty(), "{content}: {found:?}");
        assert_eq!(found.rectangles[0], [10.0, 10.0, 50.0, 40.0], "{content}");
        if found.rectangles.len() == 2 {
            assert_eq!(
                found.rectangles[1],
                [100.0, 100.0, 120.0, 130.0],
                "{content}"
            );
        }
    }
    // Turned 30 degrees, among other subpaths: a quad.
    let found = drawings(
        &page(
            "0.866025 0.5 -0.5 0.866025 100 100 cm \
             0 0 m 10 0 l 10 20 l 0 20 l h 30 0 m 40 0 l S",
        ),
        0,
        false,
    )
    .unwrap();
    assert!(found.rectangles.is_empty());
    assert_eq!((found.quads.len(), found.lines.len()), (1, 1), "{found:?}");
}

#[test]
fn a_box_filled_then_stroked_counts_once_however_its_corners_are_written() {
    // Filled with `re` (its far corner computed), stroked with its corners written.
    let found = drawings(
        &page("42.45 547.961 6.7 6.7 re f 42.45 547.961 m 49.15 547.961 l 49.15 554.661 l 42.45 554.661 l h S"),
        0,
        false,
    )
    .unwrap();
    assert_eq!(found.rectangles.len(), 1, "{found:?}");
    assert!(found.lines.is_empty());
    // Stroked where it was filled, written under another matrix.
    let found = drawings(
        &page("42.45 547.961 6.7 6.7 re f q 1 0 0 1 18 0 cm 24.45 547.961 6.7 6.7 re S Q"),
        0,
        false,
    )
    .unwrap();
    assert_eq!(found.rectangles.len(), 1, "{found:?}");
}

#[test]
fn closing_a_subpath_draws_its_last_side_and_filling_one_does_not() {
    let lines = |content: &str| drawings(&page(content), 0, false).unwrap().lines;
    // A triangle closed: three sides.
    assert_eq!(
        lines("10 10 m 50 10 l 30 40 l h S"),
        [
            [10.0, 10.0, 50.0, 10.0],
            [50.0, 10.0, 30.0, 40.0],
            [30.0, 40.0, 10.0, 10.0]
        ]
    );
    // Open, stroked: two. Filled without `h`: two as well, as in MuPDF.
    assert_eq!(lines("10 10 m 50 10 l 30 40 l S").len(), 2);
    assert_eq!(lines("10 10 m 50 10 l 30 40 l f").len(), 2);
    // Every subpath closed gets its side, with `s` and `b` too; a box that is no rectangle
    // (its sides do not run along the axes in turn) is four lines.
    assert_eq!(
        lines("10 10 m 50 10 l 30 40 l h 60 10 m 90 10 l 70 40 l s").len(),
        6
    );
    assert_eq!(lines("10 10 m 50 10 l 30 40 l b").len(), 3);
    assert_eq!(lines("10 10 m 50 40 l 50 10 l 10 40 l h S").len(), 4);
    assert_eq!(lines("10 10 m 50 10 l 45 40 l 15 40 l h S").len(), 4);
    // A closed subpath already back at its start gets no empty side; a curve's end is closed.
    assert_eq!(lines("10 10 m 50 10 l 30 40 l 10 10 l h S").len(), 3);
    assert_eq!(
        lines("10 10 m 50 10 l 50 40 30 50 10 40 c h S"),
        [[10.0, 10.0, 50.0, 10.0], [10.0, 40.0, 10.0, 10.0]]
    );
    // What follows a closed subpath without a move starts at its start.
    assert_eq!(
        lines("10 10 m 50 10 l 30 40 l h 10 60 l S")[3],
        [10.0, 10.0, 10.0, 60.0]
    );
}

#[test]
fn a_page_without_paths_has_nothing_and_a_missing_page_is_refused() {
    let doc = page("BT ET");
    assert_eq!(
        drawings(&doc, 0, false).unwrap(),
        sealwax_text::Drawings::default()
    );
    assert!(drawings(&doc, 1, false).is_err());
}

#[test]
fn annotations_paint_their_appearances_when_asked() {
    let stream = |content: &str| {
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
    };
    let doc = build(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] >> >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
         /Annots [5 0 R 6 0 R 9 0 R] >>"
            .to_owned(),
        "<< /Length 13 >>\nstream\n0 0 m 9 9 l S\nendstream".to_owned(),
        // A comment drawing one line, a check box drawing its border, a hidden one.
        "<< /Type /Annot /Subtype /Square /Rect [10 10 30 30] /F 4 /AP << /N 7 0 R >> >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (c) /Rect [50 50 70 70] /F 4 \
         /AP << /N 8 0 R >> >>"
            .to_owned(),
        stream("0 0 m 20 0 l S"),
        stream("0 0 20 20 re S"),
        "<< /Type /Annot /Subtype /Square /Rect [100 100 120 120] /F 2 /AP << /N 7 0 R >> >>"
            .to_owned(),
    ]);
    let without = drawings(&doc, 0, false).unwrap();
    assert_eq!(without.lines, [[0.0, 0.0, 9.0, 9.0]]);
    assert!(without.rectangles.is_empty());
    let with = drawings(&doc, 0, true).unwrap();
    assert_eq!(with.lines, [[0.0, 0.0, 9.0, 9.0], [10.0, 10.0, 30.0, 10.0]]);
    assert_eq!(with.rectangles, [[50.0, 50.0, 70.0, 70.0]]);
}

#[test]
fn a_shading_paints_no_lines() {
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] \
                   /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >>";
    let content = "q 10 10 40 30 re W n /Sh1 sh Q /Pattern cs /P1 scn 60 60 40 30 re f \
                   /Pattern CS /P1 SCN 10 100 m 50 100 l S 0 g 100 100 20 20 re f";
    let doc = build(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
         /Resources << /Shading << /Sh1 5 0 R >> /Pattern << /P1 6 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        shading.to_owned(),
        "<< /PatternType 2 /Shading 5 0 R >>".to_owned(),
    ]);
    let found = drawings(&doc, 0, false).unwrap();
    // `sh` in a clip, a rectangle filled and a line stroked with a shading: nothing. A rectangle
    // filled with a colour: itself.
    assert!(found.lines.is_empty(), "{found:?}");
    assert_eq!(found.rectangles, [[100.0, 100.0, 120.0, 120.0]]);
}

#[test]
fn a_box_filled_with_its_fourth_side_back_to_its_start_is_four_lines_as_in_mupdf() {
    let found = |content: &str| {
        let found = drawings(&page(content), 0, false).unwrap();
        (found.lines.len(), found.rectangles.len())
    };
    let open = "10 10 m 50 10 l 50 40 l 10 40 l 10 10 l";
    // Filled, alone in its path (which hayro hands over as a rectangle) or not, with `h` or not:
    // four lines. Filled and stroked: four lines too, the stroke repeating the fill.
    for paint in ["f", "f*", "F", "B", "h f"] {
        assert_eq!(found(&format!("{open} {paint}")), (4, 0), "{paint}");
    }
    assert_eq!(
        found(&format!("{open} 60 10 m 90 10 l 90 40 l 60 40 l 60 10 l f")),
        (8, 0)
    );
    // Only stroked, it is a rectangle; closed by `h` after three sides, a rectangle filled too.
    assert_eq!(found(&format!("{open} S")), (0, 1));
    assert_eq!(found("10 10 m 50 10 l 50 40 l 10 40 l h f"), (0, 1));
    assert_eq!(found("10 10 40 30 re f"), (0, 1));
    // The lines keep the order the content draws them in.
    let lines = drawings(&page(&format!("{open} f")), 0, false)
        .unwrap()
        .lines;
    assert_eq!(
        lines,
        [
            [10.0, 10.0, 50.0, 10.0],
            [50.0, 10.0, 50.0, 40.0],
            [50.0, 40.0, 10.0, 40.0],
            [10.0, 40.0, 10.0, 10.0]
        ]
    );
}

#[test]
fn a_curve_whose_control_points_lie_on_its_ends_is_a_line() {
    let lines = |content: &str| drawings(&page(content), 0, false).unwrap().lines;
    for curve in [
        "10 10 m 10 10 50 50 50 50 c S",
        "10 10 m 10 10 10 10 50 50 c S",
        "10 10 m 50 50 50 50 50 50 c S",
    ] {
        assert_eq!(lines(curve), [[10.0, 10.0, 50.0, 50.0]], "{curve}");
    }
    // A real curve, one with a single control point on an end, or one a hair away: no line.
    for curve in [
        "10 10 m 20 40 40 60 50 50 c S",
        "10 10 m 10 10 30 60 50 50 c S",
        "10 10 m 20 40 50 50 50 50 c S",
        "10 10 m 10.001 10 50 50 50 50 c S",
    ] {
        assert!(lines(curve).is_empty(), "{curve}");
    }
    // Four degenerate curves closing a box make a rectangle, as four lines would.
    let found = drawings(
        &page("10 10 m 10 10 50 10 50 10 c 50 10 50 40 50 40 c 50 40 10 40 10 40 c h f"),
        0,
        false,
    )
    .unwrap();
    assert_eq!(found.rectangles, [[10.0, 10.0, 50.0, 40.0]]);
}
