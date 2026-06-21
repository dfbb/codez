use crate::connector::ConnError;

/// Transform plugin: operates on codex's native ResponsesApiRequest, protocol-agnostic.
/// No implementation in v1; the compressor will live here in the future.
pub trait TransformPlugin: Send + Sync {
    fn transform(&self, req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>;
}

/// Run all plugins in order; abort on the first failure.
pub fn run_transforms(
    plugins: &[Box<dyn TransformPlugin>],
    req: &mut codex_api::ResponsesApiRequest,
) -> Result<(), ConnError> {
    for p in plugins {
        p.transform(req)?;
    }
    Ok(())
}

/// v1 default plugin set: empty.
pub fn default_plugins() -> Vec<Box<dyn TransformPlugin>> {
    crate::transform::plugins()
}
