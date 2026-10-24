//! A 3D machine simulator for testing touch probing.
//!
//! This crate is the foundation layer: geometry types, mesh queries, STEP
//! import and the signal seam. The machine model, the HAL component and the UI
//! are built on top of it in later waves.
//!
//! Modules:
//!
//! - [types] - the shared contract: [types::Bounds], [types::Transform],
//!   [types::TriMesh], [types::Sphere], [types::Capsule], [types::Contact] and
//!   [types::MeshIndex].
//! - [mesh] - closest-point primitives on triangles and segments.
//! - [bvh] - the BVH behind [types::MeshIndex].
//! - [stl] - minimal binary/ASCII STL reader for the freecadcmd fallback.
//! - [step] - STEP import through the OCCT helper or freecadcmd, with a disk
//!   cache.
//! - [sink] - [sink::Sim3dSignal], the seam to the machine, and
//!   [sink::MockSignal].
//!
//! Cutting material is not simulated: the simulator models the machine, the
//! probe and the cutters, and reports contact and collision.

pub mod bvh;
pub mod config;
pub mod engine;
pub mod hal;
pub mod machine;
pub mod mesh;
pub mod model;
pub mod sink;
pub mod step;
pub mod stl;
pub mod tools;
pub mod types;
pub mod ui;
pub mod viewer;

pub use bvh::MeshIndex;
pub use mesh::Closest;
pub use sink::{MockSignal, Sim3dSignal};
pub use step::{StepBackend, StepError, StepImporter, LINC_MESH_FORMAT};
pub use stl::StlError;
pub use types::{Bounds, Capsule, Contact, Sphere, Transform, TriMesh, P3};

/// The crate version, as reported by the command line.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
