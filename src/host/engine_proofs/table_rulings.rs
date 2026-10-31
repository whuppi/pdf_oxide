//! Table detection holds the rulings of a page, never its paths.
//!
//! A page of ruled tables is built here in two forms: as a document, for the
//! public path (`extract_paths`, `extract_tables`), and as paths handed
//! straight to a `RulingGrid`, for sizes no document test would read in time.

use super::support::{pdf_from_objects, stream_object};
use crate::document::PdfDocument;
use crate::elements::PathContent;
use crate::geometry::Rect;
use crate::host::ruling_grid::RulingGrid;
use crate::layout::TextSpan;
use crate::structure::spatial_table_detector::{detect_tables, TableDetectionConfig, TableStrategy};
use std::time::Instant;

/// One-page document around a content stream, with a large page and Helvetica as F1.
fn page_with(content: &str, size: u32) -> Vec<u8> {
    pdf_from_objects(&[
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {size} {size}] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n"
        )
        .into_bytes(),
        stream_object(4, "", content.as_bytes()),
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
    ])
}

/// Rows and columns of a drawn table, and the size of its cells, in points.
const TABLE_ROWS: usize = 14;
const TABLE_COLS: usize = 14;
const CELL_W: f32 = 24.0;
const CELL_H: f32 = 12.0;

/// The content of one table with its lower left corner at (x, y): a ruling
/// for every row and column edge (15 + 15 = 30 primitives) and a word in each cell.
fn table_content(x: f32, y: f32, out: &mut String) {
    for r in 0..=TABLE_ROWS {
        let yy = y + r as f32 * CELL_H;
        out.push_str(&format!("{x} {yy} m {} {yy} l S\n", x + TABLE_COLS as f32 * CELL_W));
    }
    for c in 0..=TABLE_COLS {
        let xx = x + c as f32 * CELL_W;
        out.push_str(&format!("{xx} {y} m {xx} {} l S\n", y + TABLE_ROWS as f32 * CELL_H));
    }
    for r in 0..TABLE_ROWS {
        for c in 0..TABLE_COLS {
            out.push_str(&format!(
                "BT /F1 6 Tf {} {} Td ({}.5) Tj ET\n",
                x + c as f32 * CELL_W + 3.0,
                y + r as f32 * CELL_H + 3.0,
                r * TABLE_COLS + c
            ));
        }
    }
}

/// Sixty 14 x 14 tables, 30 primitives each: 1,800 in all, on one page.
fn sixty_tables() -> Vec<u8> {
    let mut content = String::from("0.5 w\n");
    for n in 0..60 {
        table_content(60.0 + (n % 6) as f32 * 400.0, 60.0 + (n / 6) as f32 * 220.0, &mut content);
    }
    page_with(&content, 2500)
}

/// No Dart battery sees it: a battery asserts what one small page returns,
/// and the cap this replaces counted primitives, so only a page with more of
/// them than the cap tells a detector that reads them all from one that reads
/// the first 1,500.
#[test]
fn ruled_page_over_fifteen_hundred_primitives_detects_every_table() {
    let doc = PdfDocument::from_bytes(sixty_tables()).unwrap();
    assert_eq!(doc.extract_paths(0).unwrap().len(), 1_800, "the page holds 1,800 primitives");
    let tables = doc.extract_tables(0).unwrap();
    assert_eq!(tables.len(), 60, "every table is found, not the ones among the first 1,500 paths");
    assert!(tables.iter().all(|t| t.rows.len() == TABLE_ROWS && t.col_count == TABLE_COLS));

    // The text and markdown readers took the first 1,500 paths for the page's
    // rulings and found no table at all when the page held more.
    let markdown = doc.to_markdown(0, &crate::converters::ConversionOptions::default()).unwrap();
    let separators = markdown
        .lines()
        .filter(|l| l.contains("---") && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')))
        .count();
    assert_eq!(separators, 60, "every table is a table in the markdown too");
}

/// No Dart battery sees it: a battery cannot tell the paths a sink was handed
/// from the ones a `Vec` keeps, and the whole point of the sink is that the
/// caller decides what is kept.
#[test]
fn a_sink_sees_the_paths_extract_paths_returns_in_order() {
    let doc = PdfDocument::from_bytes(sixty_tables()).unwrap();
    let kept = doc.extract_paths(0).unwrap();
    let mut seen: Vec<PathContent> = Vec::new();
    doc.extract_paths_into(0, &mut |p| seen.push(p)).unwrap();
    assert_eq!(kept.len(), seen.len());
    assert!(kept.iter().zip(&seen).all(|(a, b)| a.bbox == b.bbox && a.operations == b.operations));
    // A counting sink keeps nothing and sees every path.
    let mut count = 0usize;
    doc.extract_paths_into(0, &mut |_| count += 1).unwrap();
    assert_eq!(count, kept.len());
}

/// No Dart battery sees it: it takes a page with ten thousand random strokes,
/// more than the cap this replaces let through, and a battery asserts on what
/// a real document shows.
#[test]
fn ten_thousand_random_short_segments_detect_no_table() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32
    };
    let mut content = String::from("0.5 w\n");
    for _ in 0..10_000 {
        let (x, y) = (50.0 + next() * 900.0, 50.0 + next() * 900.0);
        // Slanted: 25 to 65 degrees off an axis, so no stroke is a horizontal
        // or vertical rule (a page of axis-aligned noise can make a grid by
        // chance, which no detector can tell from a drawn one).
        let quadrant = (next() * 4.0).floor();
        let (len, angle) = (6.0 + next() * 8.0, (quadrant * 90.0 + 25.0 + next() * 40.0).to_radians());
        content.push_str(&format!("{x} {y} m {} {} l S\n", x + len * angle.cos(), y + len * angle.sin()));
    }
    for n in 0..400 {
        content.push_str(&format!(
            "BT /F1 8 Tf {} {} Td (w{n}) Tj ET\n",
            50.0 + next() * 900.0,
            50.0 + next() * 900.0
        ));
    }
    let doc = PdfDocument::from_bytes(page_with(&content, 1000)).unwrap();
    // Rulings only: the words alone may form a table by their alignment, and
    // this proof is about what the strokes make of one.
    let config = TableDetectionConfig {
        horizontal_strategy: TableStrategy::Lines,
        vertical_strategy: TableStrategy::Lines,
        text_fallback: false,
        ..TableDetectionConfig::default()
    };
    let tables = doc.extract_tables_with_config(0, config).unwrap();
    let found: Vec<_> = tables.iter().map(|t| (t.rows.len(), t.col_count, t.bbox)).collect();
    assert!(tables.is_empty(), "tables found among random strokes: {found:?}");
}

/// A span at a point, for a detector that reads a word per cell.
fn word_at(x: f32, y: f32) -> TextSpan {
    TextSpan { text: "w".to_string(), bbox: Rect::new(x, y, 6.0, 8.0), font_size: 8.0, ..Default::default() }
}

/// `tables` tables of 3 x 3 cells in a repeating lattice of pitch 120: each
/// 8 rulings and 9 numbers. Returns the tables found and the milliseconds the
/// grid and the detector took together, per 10,000 primitives.
fn milliseconds_per_ten_thousand(tables: usize) -> (usize, f64) {
    let started = Instant::now();
    let per_row = 300usize;
    let bounds = Rect::new(0.0, 0.0, 120.0 * per_row as f32, 120.0 * (tables / per_row + 1) as f32);
    let mut grid = RulingGrid::new(bounds);
    let mut spans = Vec::with_capacity(tables * 9);
    for n in 0..tables {
        let (x, y) = ((n % per_row) as f32 * 120.0 + 10.0, (n / per_row) as f32 * 120.0 + 10.0);
        for i in 0..=3 {
            let d = i as f32 * 30.0;
            grid.add(&PathContent::line(x, y + d, x + 90.0, y + d));
            grid.add(&PathContent::line(x + d, y, x + d, y + 90.0));
        }
        for r in 0..3 {
            for c in 0..3 {
                spans.push(word_at(x + c as f32 * 30.0 + 8.0, y + r as f32 * 30.0 + 10.0));
            }
        }
    }
    assert!(!grid.refused());
    let primitives = grid.primitives();
    let found = detect_tables(&spans, &grid, &TableDetectionConfig::default());
    let count = found.len();
    std::hint::black_box(&found);
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    (count, ms / (primitives as f64 / 10_000.0))
}

/// Milliseconds per 10,000 primitives at `tables` tables: the quicker of
/// `runs` runs, so a slow start does not stand for the size. Every table must be found.
fn best_milliseconds(tables: usize, runs: usize) -> f64 {
    (0..runs)
        .map(|_| {
            let (found, ms) = milliseconds_per_ten_thousand(tables);
            assert_eq!(found, tables, "every table of the lattice is found");
            ms
        })
        .fold(f64::MAX, f64::min)
}

#[cfg(not(debug_assertions))]
/// No Dart battery sees it: only a comparison of two sizes shows growth, and
/// a million rulings is too many for a document test. Compiled for release
/// only, since the debug profile runs it for minutes.
#[test]
fn a_hundred_thousand_and_a_million_rulings_cost_the_same_per_primitive() {
    let small = best_milliseconds(12_500, 2);
    let large = best_milliseconds(125_000, 1);
    eprintln!("per 10k primitives: 100k rulings {small:.1} ms, 1M rulings {large:.1} ms");
    assert!(large / small < 1.3, "{small:.1} ms at 100k and {large:.1} ms at 1M per 10k primitives");
}

#[cfg(debug_assertions)]
/// No Dart battery sees it, as above. The debug profile reads a smaller pair.
#[test]
fn ten_thousand_and_a_hundred_thousand_rulings_cost_about_the_same_per_primitive() {
    let small = best_milliseconds(1_250, 2);
    let large = best_milliseconds(12_500, 1);
    eprintln!("per 10k primitives: 10k rulings {small:.1} ms, 100k rulings {large:.1} ms");
    assert!(large / small < 1.6, "{small:.1} ms at 10k and {large:.1} ms at 100k per 10k primitives");
}
