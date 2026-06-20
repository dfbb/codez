use crate::connector::ConnError;

/// 变换插件：作用于 codex 原生 ResponsesApiRequest，协议无关。
/// v1 无实现；将来 compressor 落在这里。
pub trait TransformPlugin: Send + Sync {
    fn transform(&self, req: &mut codex_api::ResponsesApiRequest) -> Result<(), ConnError>;
}

/// 有序执行所有插件；任一失败即中止。
pub fn run_transforms(
    plugins: &[Box<dyn TransformPlugin>],
    req: &mut codex_api::ResponsesApiRequest,
) -> Result<(), ConnError> {
    for p in plugins {
        p.transform(req)?;
    }
    Ok(())
}

/// v1 默认插件集：空。
pub fn default_plugins() -> Vec<Box<dyn TransformPlugin>> {
    crate::transform::plugins()
}
