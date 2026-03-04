//! ark-core: 共享底座
//!
//! 包含事件系统、状态图引擎、规则引擎等核心组件
//! 供 agent 和 hub 共同使用

pub mod event;
pub mod graph;
pub mod rules;

// 重新导出常用类型
pub use event::{Event, EventBus, EventType};
pub use graph::{Edge, EdgeType, Node, NodeType, StateGraph};
