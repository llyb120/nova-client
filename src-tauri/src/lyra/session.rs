//! 主子 agent 调度已暂时移除：不再提供 spawn_agent / send_message / followup_task /
//! wait_agent / list_agents / interrupt_agent / close_agent，本模块保留为空的接缝，
//! 让 `mod session;` 与 `lyra::session::` 引用仍可编译；恢复时在这里重新实现。
