use crate::connector::ConnError;

/// Transform plugin: acts on codex native ResponsesApiRequest, protocol-agnostic.
/// v1 has no implementation; compressor will be placed here in the future.
pub trait TransformPlugin: Send + Sync {
    fn transform(&self, req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>;
}

/// Execute all plugins in order; stop on first failure.
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
