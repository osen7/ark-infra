use crate::scene::types::{AnalysisResult, SceneType};
use ark_core::graph::StateGraph;

/// 场景分析器 trait
#[async_trait::async_trait]
pub trait SceneAnalyzer: Send + Sync {
    /// 分析场景
    async fn analyze(&self, graph: &StateGraph, target: &str) -> AnalysisResult;

    /// 获取场景类型
    fn scene_type(&self) -> SceneType;
}

/// 场景注册表
pub struct SceneRegistry {
    analyzers: Vec<Box<dyn SceneAnalyzer>>,
}

impl SceneRegistry {
    pub fn new() -> Self {
        Self {
            analyzers: Vec::new(),
        }
    }

    pub fn register<A: SceneAnalyzer + 'static>(&mut self, analyzer: A) {
        self.analyzers.push(Box::new(analyzer));
    }

    pub fn get_analyzer(&self, scene: SceneType) -> Option<&dyn SceneAnalyzer> {
        self.analyzers
            .iter()
            .find(|a| a.scene_type() == scene)
            .map(|a| a.as_ref())
    }

    pub fn all_analyzers(&self) -> &[Box<dyn SceneAnalyzer>] {
        &self.analyzers
    }
}

impl Default for SceneRegistry {
    fn default() -> Self {
        Self::new()
    }
}
