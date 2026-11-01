//! Mesh shadings (types 4 to 7) in the redaction pass. A mesh is decided and
//! rewritten primitive by primitive: a triangle or patch whose box meets a
//! padded region is dropped, the rest are written again with full vertices
//! at the source's bit depths, so the copy holds no coordinate or colour of
//! anything under a region.

use std::collections::HashMap;
use std::io::Write;

use crate::content::graphics_state::Matrix;
use crate::error::{Error, Result};
use crate::geometry::Rect;
use crate::object::{Object, ObjectRef};
use crate::redaction::region::RegionSet;
use crate::rendering::mesh_shading::{self, MeshFilter};

/// A mesh shading stream, with the entries its decoder reads resolved.
#[derive(Clone)]
pub struct MeshSource {
    /// The shading's object.
    pub reference: ObjectRef,
    /// The shading stream; its dictionary holds the bit depths and `/Decode`
    /// as direct values.
    pub object: Object,
    /// `/ShadingType`: 4, 5, 6 or 7.
    pub shading_type: i64,
}

/// A shading pattern (`/PatternType 2`) whose shading is a mesh.
#[derive(Clone)]
pub struct MeshPattern {
    /// The pattern dictionary's object.
    pub reference: ObjectRef,
    /// The pattern's `/Matrix`: pattern space to the default space of the
    /// stream that uses it.
    pub matrix: Matrix,
    /// The pattern's shading.
    pub mesh: MeshSource,
}

impl MeshSource {
    fn dict(&self) -> Result<&HashMap<String, Object>> {
        match &self.object {
            Object::Stream { dict, .. } => Ok(dict),
            _ => Err(Error::Unsupported(
                "a mesh shading is not a stream; refusing to redact a mesh it cannot read".to_string(),
            )),
        }
    }
}

/// Streams the primitives of `mesh` through the regions, with the mesh placed
/// by `to_page`, and writes the survivors to `out`.
///
/// # Errors
/// [`Error::Unsupported`] when the mesh cannot be read, [`Error::Io`] when
/// `out` fails.
pub(super) fn filter<W: Write>(
    mesh: &MeshSource,
    to_page: &Matrix,
    regions: &RegionSet,
    padding: f32,
    out: &mut W,
) -> Result<MeshFilter> {
    let dict = mesh.dict()?;
    let data = crate::host::content::parts::decoded_reader(&mesh.object)?;
    let holes: Vec<Rect> = regions.regions.iter().map(|r| r.padded_rect(padding)).collect();
    let mut meets = |points: &[(f32, f32)]| {
        let mut lo = (f32::INFINITY, f32::INFINITY);
        let mut hi = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for &(x, y) in points {
            let p = to_page.transform_point(x, y);
            if !(p.x.is_finite() && p.y.is_finite()) {
                // A point no box can bound is dropped, never kept.
                return true;
            }
            lo = (lo.0.min(p.x), lo.1.min(p.y));
            hi = (hi.0.max(p.x), hi.1.max(p.y));
        }
        let bbox = Rect::from_points(lo.0, lo.1, hi.0, hi.1);
        holes.iter().any(|hole| hole.intersects(&bbox))
    };
    mesh_shading::filter_mesh(dict, mesh.shading_type, data, &mut meets, out)
}

/// Writes the copy of `mesh` without the primitives that meet a region.
///
/// # Errors
/// As [`filter`].
pub(crate) fn write_filtered<W: Write>(
    mesh: &MeshSource,
    to_page: &Matrix,
    regions: &RegionSet,
    padding: f32,
    out: &mut W,
) -> Result<MeshFilter> {
    filter(mesh, to_page, regions, padding, out)
}

/// The dictionary of the copy [`write_filtered`] writes: the source's, a
/// lattice becoming free-form triangles. The stream entries are the writer's.
///
/// # Errors
/// [`Error::Unsupported`] when the mesh cannot be read.
pub(crate) fn filtered_dict(mesh: &MeshSource) -> Result<HashMap<String, Object>> {
    mesh_shading::filtered_mesh_dict(mesh.dict()?, mesh.shading_type).ok_or_else(|| {
        Error::Unsupported(
            "a mesh shading's parameters cannot be read; refusing to redact a mesh it cannot filter"
                .to_string(),
        )
    })
}
