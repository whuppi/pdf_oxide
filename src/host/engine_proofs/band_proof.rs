//! A page cut into bands renders the pixels the whole canvas does.
//!
//! Each fixture is rendered with the band height forced to 1, 17 and 256 rows
//! and as one band, at 36, 72, 150 and 300 dpi, on a transparent background so
//! a pixel's alpha is its coverage. Until the rasteriser's edge clipping is
//! made exact across a band's top, a pixel may differ only where it is partly
//! covered in one of the two renders, or is an edge between two flat areas:
//! every other difference fails. The counts are printed per fixture.

use super::support::{pdf_from_objects, stream_object};
use crate::cancel::CancelCheck;
use crate::document::PdfDocument;
use crate::rendering::{PageRenderer, RenderOptions};

struct Fixture {
    name: &'static str,
    content: String,
    resources: String,
    annots: String,
    /// Objects numbered from 5.
    extra: Vec<Vec<u8>>,
    /// `Some(levels)` when colour varies smoothly, so a neighbour test cannot
    /// tell an edge from an interior: only partial coverage, or a step of at
    /// most `levels` in every channel (a gradient value on a rounding tie,
    /// moved by float error), excuses a difference.
    smooth: Option<u8>,
}

impl Fixture {
    fn new(name: &'static str, content: &str) -> Self {
        Self {
            name,
            content: content.to_string(),
            resources: String::new(),
            annots: String::new(),
            extra: Vec::new(),
            smooth: None,
        }
    }

    fn pdf(&self) -> Vec<u8> {
        let mut objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Resources << {} >> {} >>\nendobj\n",
                self.resources, self.annots
            )
            .into_bytes(),
            stream_object(4, "", self.content.as_bytes()),
        ];
        objects.extend(self.extra.iter().cloned());
        pdf_from_objects(&objects)
    }
}

const HELVETICA: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>";

fn fixtures() -> Vec<Fixture> {
    let mut all = Vec::new();

    all.push(Fixture::new(
        "paths",
        "0.9 0.2 0.2 rg 10.3 10.7 60.5 40.2 re f\n\
         0.2 0.4 0.9 rg 80 20 m 130 20 l 105 70 l f\n\
         0.1 0.7 0.3 rg 20 80 m 60 140 100 140 140 80 c 100 100 60 100 20 80 c f\n\
         0.5 0.5 0.1 rg 150 100 m 190 100 l 190 140 l 150 140 l h 160 110 m 180 110 l \
         180 130 l 160 130 l h f*\n\
         0 0 0 rg 100 100 m 120 190 l 135 100 l 170 160 l 105 150 l h f\n\
         0.3 g 0 120.5 200 0.5 re f\n\
         0 0.3 0.6 rg 99.5 0 1 200 re f\n\
         q 0.866 0.5 -0.5 0.866 100 100 cm 0.6 0.1 0.6 rg 0 0 80 3 re f Q\n\
         q 0.17 0.98 -0.98 0.17 150 10 cm 0.1 0.5 0.5 rg 0 0 120 2.5 re f Q\n",
    ));

    all.push(Fixture::new(
        "strokes",
        "2.7 w 1 J 1 j 0 0.5 0 RG 10 10 m 190 190 l S\n\
         5 w 0 J 0 j 10 M 0.5 0 0 RG 20 180 m 60 120 l 100 180 l S\n\
         0 w 0 0 0 RG 10 30 m 190 52 l S\n\
         0.3 w 0 0 1 RG 5 190 m 195 20 l S\n\
         10 w 2 J 2 j 0.2 0.2 0.8 RG 20 100 m 70 190 130 10 180 100 c S\n\
         1.5 w 0 J 0 j 0 0 0 RG 30.5 30.5 100 100 re S\n\
         3 w 1 J 1 j 0.7 0.1 0.1 RG 150 150 m 190 190 l 150 190 l h S\n\
         8 w 0 J 0 j 1 M 0 0.4 0.4 RG 60 60 m 150 80 l 70 100 l S\n",
    ));

    all.push(Fixture::new(
        "dashes",
        "[6 3] 0 d 2 w 0 0 0 RG 10 20 m 190 40 l S\n\
         [1 4] 2 d 1 J 3 w 0.8 0 0 RG 10 60 m 190 100 l S\n\
         [10 5 2 5] 0 d 1.5 w 0 J 0 0.5 0 RG 20 190 m 180 10 l S\n\
         [3] 1 d 2 w 0 0 0.8 RG 100 100 m 100 190 150 190 190 100 c S\n\
         [4 2] 0 d 1 w 0 0 0 RG 0.866 0.5 -0.5 0.866 100 100 cm 0 0 80 40 re S\n",
    ));

    let mut text = Fixture::new(
        "text",
        "BT /F1 24 Tf 10 170 Td (Banded Text) Tj ET\n\
         BT /F1 9 Tf 10 150 Td (The quick brown fox jumps over the lazy dog) Tj ET\n\
         BT /F1 36 Tf 0.866 0.5 -0.5 0.866 40 40 Tm 0.1 0.2 0.7 rg (Rot) Tj ET\n\
         BT /F1 18 Tf 1 Tr 1 w 10 100 Td 0 0 0 RG (Stroke) Tj ET\n\
         BT /F1 22 Tf 2 Tr 1.5 w 100 110 Td 0.9 0.8 0.1 rg 0.5 0 0 RG (Both) Tj ET\n\
         BT /F1 14 Tf 0 Tr 10 70 Td 0 g [(A) -300 (B) 200 (C) -100 (Spaced)] TJ ET\n\
         BT /F1 11 Tf 10 50 Td 14 TL (First) Tj (Second) ' T* (Third) Tj ET\n",
    );
    text.resources = HELVETICA.to_string();
    all.push(text);

    let mut clips = Fixture::new(
        "clips",
        "q 100 20 m 150 20 180 50 180 100 c 180 150 150 180 100 180 c 50 180 20 150 20 100 c \
         20 50 50 20 100 20 c h W n 1 0 0 rg 0 0 200 200 re f 0 0 1 rg 0 90 200 20 re f Q\n\
         q 20 20 80 80 re W n 70 70 80 80 re W n 0 0.6 0 rg 0 0 200 200 re f Q\n\
         q BT /F1 60 Tf 7 Tr 10 130 Td (CLIP) Tj ET 0.8 0.1 0.8 rg 0 0 200 200 re f Q\n\
         q 10 10 m 190 10 l 100 190 l h W* n 0.1 0.1 0.1 RG 4 w 0 0 m 200 200 l S Q\n",
    );
    clips.resources = HELVETICA.to_string();
    all.push(clips);

    let function = "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>";
    let mut axial = Fixture::new(
        "axial",
        "q 20 20 160 160 re W n /S1 sh Q\n\
         q 10 10 m 190 10 l 100 190 l h W n /S2 sh Q\n",
    );
    axial.resources = format!(
        "/Shading << /S1 << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [10 10 190 190] \
         /Function {function} /Extend [true true] >> \
         /S2 << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [40 60 160 90] \
         /Function {function} /Extend [false true] >> >>"
    );
    axial.smooth = Some(1);
    all.push(axial);

    let mut radial = Fixture::new(
        "radial",
        "q 0 0 200 200 re W n /S1 sh Q\n\
         q 10 100 180 90 re W n /S2 sh Q\n",
    );
    radial.resources = format!(
        "/Shading << /S1 << /ShadingType 3 /ColorSpace /DeviceRGB /Coords [100 100 10 100 100 90] \
         /Function {function} /Extend [false false] >> \
         /S2 << /ShadingType 3 /ColorSpace /DeviceRGB /Coords [90 120 5 100 110 60] \
         /Function {function} /Extend [true false] >> >>"
    );
    radial.smooth = Some(64);
    all.push(radial);

    all.push(mesh_fixture());
    all.push(forms_fixture());
    all.push(annotation_fixture());
    all.push(image_fixture());
    all
}

/// Types 4 to 7 in one page, each in its own quarter, every shape crossing
/// the cuts.
fn mesh_fixture() -> Fixture {
    let decode = "/Decode [0 200 0 200 0 1 0 1 0 1]";
    let common = "/ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8";

    let vertex = |flag: Option<u8>, x: u8, y: u8, rgb: [u8; 3]| {
        let mut v = Vec::new();
        v.extend(flag);
        v.extend([x, y]);
        v.extend(rgb);
        v
    };

    let mut type4 = Vec::new();
    type4.extend(vertex(Some(0), 10, 20, [255, 0, 0]));
    type4.extend(vertex(Some(0), 120, 10, [0, 255, 0]));
    type4.extend(vertex(Some(0), 60, 150, [0, 0, 255]));
    type4.extend(vertex(Some(1), 200, 140, [255, 255, 0]));
    type4.extend(vertex(Some(2), 220, 60, [0, 255, 255]));

    let mut type5 = Vec::new();
    for (row, y) in [20u8, 110, 200].iter().enumerate() {
        for (col, x) in [20u8, 130, 240].iter().enumerate() {
            let rgb = [(row * 120) as u8, (col * 120) as u8, 200 - (row * col * 40) as u8];
            type5.extend(vertex(None, *x, *y, rgb));
        }
    }

    let point = |x: u8, y: u8| vec![x, y];
    let coons: [(u8, u8); 12] = [
        (40, 40),
        (30, 100),
        (50, 160),
        (40, 220),
        (100, 230),
        (160, 210),
        (220, 220),
        (230, 160),
        (210, 100),
        (220, 40),
        (160, 30),
        (100, 50),
    ];
    let tensor: [(u8, u8); 4] = [(90, 90), (90, 170), (170, 170), (170, 90)];
    let colours = [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
    let patch = |with_interior: bool| {
        let mut p = vec![0u8];
        for (x, y) in coons {
            p.extend(point(x, y));
        }
        if with_interior {
            for (x, y) in tensor {
                p.extend(point(x, y));
            }
        }
        for c in colours {
            p.extend(c);
        }
        p
    };

    let mut fixture = Fixture::new(
        "mesh",
        "/M4 sh\n\
         q 0.5 0 0 0.5 100 0 cm /M5 sh Q\n\
         q 0.5 0 0 0.5 0 100 cm /M6 sh Q\n\
         q 0.5 0 0 0.5 100 100 cm /M7 sh Q\n",
    );
    fixture.resources = "/Shading << /M4 5 0 R /M5 6 0 R /M6 7 0 R /M7 8 0 R >>".to_string();
    fixture.extra = vec![
        stream_object(5, &format!("/ShadingType 4 {common} /BitsPerFlag 8 {decode}"), &type4),
        stream_object(
            6,
            &format!("/ShadingType 5 {common} /VerticesPerRow 3 {decode}"),
            &type5,
        ),
        stream_object(
            7,
            &format!("/ShadingType 6 {common} /BitsPerFlag 8 {decode}"),
            &patch(false),
        ),
        stream_object(
            8,
            &format!("/ShadingType 7 {common} /BitsPerFlag 8 {decode}"),
            &patch(true),
        ),
    ];
    fixture.smooth = Some(1);
    fixture
}

/// Forms that straddle, sit inside and miss the cuts, one of them a
/// transparency group.
fn forms_fixture() -> Fixture {
    let mut fixture = Fixture::new(
        "forms",
        "q 1 0 0 1 10 10 cm /Fm1 Do Q\n\
         q 1 0 0 1 80 100 cm /Fm1 Do Q\n\
         q 0.7 0.7 -0.7 0.7 100 30 cm /Fm1 Do Q\n\
         q 1 0 0 1 20 140 cm /Fm2 Do Q\n\
         q 1 0 0 1 400 400 cm /Fm1 Do Q\n",
    );
    fixture.resources = "/XObject << /Fm1 5 0 R /Fm2 6 0 R >> \
         /ExtGState << /Half << /ca 0.5 >> >>"
        .to_string();
    fixture.extra = vec![
        stream_object(
            5,
            "/Type /XObject /Subtype /Form /BBox [0 0 60 60]",
            b"0.2 0.6 0.9 rg 0 0 60 60 re f 1 0 0 rg 20.5 20.5 30 30 re f 0 G 2 w 5 5 m 55 55 l S",
        ),
        stream_object(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 80 40] /Group << /S /Transparency /I true >>",
            b"0.9 0.1 0.1 rg 0 0 50 40 re f 0.1 0.1 0.9 rg 30 0 50 40 re f",
        ),
    ];
    fixture
}

fn annotation_fixture() -> Fixture {
    let mut fixture = Fixture::new("annotations", "0.95 g 0 0 200 200 re f\n");
    fixture.annots = "/Annots [ 5 0 R 7 0 R ]".to_string();
    fixture.extra = vec![
        b"5 0 obj\n<< /Type /Annot /Subtype /Square /Rect [20 20 120 80] /F 4 \
          /AP << /N 6 0 R >> >>\nendobj\n"
            .to_vec(),
        stream_object(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 100 60]",
            b"1 0 0 RG 3 w 1.5 1.5 97 57 re S 0.9 0.9 0 rg 10 10 30 20 re f",
        ),
        b"7 0 obj\n<< /Type /Annot /Subtype /Square /Rect [110 100 190 190] /F 4 \
          /AP << /N 8 0 R >> >>\nendobj\n"
            .to_vec(),
        stream_object(
            8,
            "/Type /XObject /Subtype /Form /BBox [0 0 80 90]",
            b"0 0.5 0 RG 2 w 1 1 78 88 re S 0 0.5 0 rg 10 10 m 70 10 l 40 80 l f",
        ),
    ];
    fixture
}

/// An upscaled image and a rotated one, the image path as it stands.
fn image_fixture() -> Fixture {
    let mut pixels = Vec::new();
    for y in 0..4u8 {
        for x in 0..4u8 {
            pixels.extend([x * 80, y * 80, 255 - x * 40 - y * 40]);
        }
    }
    let mut fixture = Fixture::new(
        "images",
        "q 100.4 0 0 100.6 20.3 70.2 cm /Im1 Do Q\n\
         q 50 30 -30 50 150 20 cm /Im1 Do Q\n\
         q 0 0 0.5 rg 40 0 0 40 20 20 cm /Mk Do Q\n",
    );
    fixture.resources = "/XObject << /Im1 5 0 R /Mk 6 0 R >>".to_string();
    fixture.extra = vec![
        stream_object(
            5,
            "/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8",
            &pixels,
        ),
        stream_object(
            6,
            "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ImageMask true \
             /BitsPerComponent 1",
            &[0xAA, 0x55, 0xAA, 0x55, 0xF0, 0xF0, 0x0F, 0x0F],
        ),
    ];
    fixture
}

fn render(doc: &PdfDocument, dpi: u32, rows: Option<u32>) -> (u32, u32, Vec<u8>) {
    let mut renderer = PageRenderer::new(RenderOptions::with_dpi(dpi).with_transparent_background());
    renderer.forced_rows = rows;
    let mut raster = Vec::new();
    let (width, height) = renderer
        .render_bands(
            doc,
            0,
            &|_, height| 0..height,
            &CancelCheck::none(),
            &mut |_, band| {
                raster.extend_from_slice(band.data());
                Ok(())
            },
        )
        .expect("renders");
    (width, height, raster)
}

#[derive(Default)]
struct Diff {
    differing: u64,
    partial: u64,
    edge: u64,
    tie: u64,
    other: u64,
    max_channel: u8,
    first: Option<(u32, u32, [u8; 4], [u8; 4])>,
    /// The first few unexcused pixels, as `(x, y, whole, cut)`.
    others: Vec<(u32, u32, [u8; 4], [u8; 4])>,
}

fn pixel(raster: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = (y as usize * width as usize + x as usize) * 4;
    raster[at..at + 4].try_into().expect("four bytes")
}

/// True when no neighbour of the pixel differs from it.
fn flat(raster: &[u8], width: u32, height: u32, x: u32, y: u32) -> bool {
    let here = pixel(raster, width, x, y);
    (y.saturating_sub(1)..=(y + 1).min(height - 1)).all(|ny| {
        (x.saturating_sub(1)..=(x + 1).min(width - 1)).all(|nx| pixel(raster, width, nx, ny) == here)
    })
}

fn compare(width: u32, height: u32, whole: &[u8], cut: &[u8], smooth: Option<u8>) -> Diff {
    let mut diff = Diff::default();
    for y in 0..height {
        for x in 0..width {
            let (a, b) = (pixel(whole, width, x, y), pixel(cut, width, x, y));
            if a == b {
                continue;
            }
            diff.differing += 1;
            diff.first.get_or_insert((x, y, a, b));
            for channel in 0..4 {
                diff.max_channel = diff.max_channel.max(a[channel].abs_diff(b[channel]));
            }
            let partly = |p: [u8; 4]| p[3] != 0 && p[3] != 255;
            if partly(a) || partly(b) {
                diff.partial += 1;
            } else if smooth.is_some_and(|levels| (0..4).all(|c| a[c].abs_diff(b[c]) <= levels)) {
                diff.tie += 1;
            } else if smooth.is_none()
                && !(flat(whole, width, height, x, y) && flat(cut, width, height, x, y))
            {
                diff.edge += 1;
            } else {
                diff.other += 1;
                if diff.others.len() < 8 {
                    diff.others.push((x, y, a, b));
                }
            }
        }
    }
    diff
}

fn prove(fixture: &Fixture) {
    let doc = PdfDocument::from_bytes(fixture.pdf()).expect("fixture parses");
    let mut failures = Vec::new();
    for dpi in [36, 72, 150, 300] {
        let (width, height, whole) = render(&doc, dpi, Some(u32::MAX));
        assert!(whole.iter().any(|&b| b != 0), "{} at {dpi} dpi drew nothing", fixture.name);
        for rows in [Some(1), Some(17), Some(256), None] {
            let (w, h, cut) = render(&doc, dpi, rows);
            assert_eq!((w, h), (width, height));
            let diff = compare(width, height, &whole, &cut, fixture.smooth);
            println!(
                "band_proof {:<12} {dpi:>3} dpi rows {:>8}: {} px, differing {} (partial coverage {}, \
                 edge {}, one-level tie {}, other {}), max channel step {}",
                fixture.name,
                rows.map_or("default".to_string(), |r| r.to_string()),
                width as u64 * height as u64,
                diff.differing,
                diff.partial,
                diff.edge,
                diff.tie,
                diff.other,
                diff.max_channel,
            );
            if let Some((x, y, a, b)) = diff.first {
                println!("    first differing pixel ({x}, {y}): whole {a:?} cut {b:?}");
            }
            for (x, y, a, b) in &diff.others {
                println!("    unexcused pixel ({x}, {y}): whole {a:?} cut {b:?}");
            }
            if diff.other > 0 {
                failures.push(format!("{dpi} dpi, rows {rows:?}: {} unexcused", diff.other));
            }
        }
    }
    assert!(failures.is_empty(), "{}: {failures:?}", fixture.name);
}

/// No Dart battery sees it: the band height is an internal knob no API sets,
/// and the property is about the pixels of one render against another.
#[test]
fn bands_equal_the_whole_canvas_on_paths_strokes_and_dashes() {
    for fixture in fixtures()
        .iter()
        .filter(|f| ["paths", "strokes", "dashes"].contains(&f.name))
    {
        prove(fixture);
    }
}

/// No Dart battery sees it: the band height is an internal knob no API sets.
#[test]
fn bands_equal_the_whole_canvas_on_text_and_clips() {
    for fixture in fixtures().iter().filter(|f| ["text", "clips"].contains(&f.name)) {
        prove(fixture);
    }
}

/// No Dart battery sees it: the band height is an internal knob no API sets.
#[test]
fn bands_equal_the_whole_canvas_on_shadings() {
    for fixture in fixtures()
        .iter()
        .filter(|f| ["axial", "radial", "mesh"].contains(&f.name))
    {
        prove(fixture);
    }
}

/// No Dart battery sees it: the band height is an internal knob no API sets.
#[test]
fn bands_equal_the_whole_canvas_on_forms_annotations_and_images() {
    for fixture in fixtures()
        .iter()
        .filter(|f| ["forms", "annotations", "images"].contains(&f.name))
    {
        prove(fixture);
    }
}
