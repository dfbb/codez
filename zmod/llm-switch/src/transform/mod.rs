use crate::pipeline::TransformPlugin;

/// v1: no transform plugins. The compressor will register here in the future.
pub fn plugins() -> Vec<Box<dyn TransformPlugin>> {
    Vec::new()
}
