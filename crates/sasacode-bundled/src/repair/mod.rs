//! Repairs for tool calls written by small models (`packages/bundled/src/repair`), and the JS string
//! semantics the bundled plugins share.

pub mod js;
pub mod json;
pub mod schema;

/// A repaired value and what was done to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Repaired<T> {
    pub value: T,
    pub fixes: Vec<String>,
}
