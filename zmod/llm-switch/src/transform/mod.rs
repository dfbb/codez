use crate::pipeline::TransformPlugin;

/// v1：无变换插件。将来 compressor 在此注册。
pub fn plugins() -> Vec<Box<dyn TransformPlugin>> {
    Vec::new()
}
