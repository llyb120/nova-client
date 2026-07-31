//! The stateful `Agent` wrapper, ported from `pi-agent-core/dist/agent.js`.
//!
//! Owns the transcript, the steering/follow-up queues, lifecycle state, and
//! listener dispatch; delegates the loop to `run_agent_loop`. The LLM and tool
//! boundaries are passed per-run so the same wrapper serves both the real
//! runtime and deterministic parity tests.

use serde_json::{json, Value};

use super::run_loop::{run_agent_loop, LoopConfig, LoopContext, PrepareNextTurnFn, StreamFn, ToolFn};

/// Port of `PendingMessageQueue`: FIFO queue whose `drain` returns everything
/// (`"all"`) or a single message (`"one-at-a-time"`).
pub struct PendingMessageQueue {
    messages: Vec<Value>,
    pub mode: String,
}

impl PendingMessageQueue {
    pub fn new(mode: &str) -> Self {
        PendingMessageQueue {
            messages: Vec::new(),
            mode: mode.to_string(),
        }
    }

    pub fn enqueue(&mut self, message: Value) {
        self.messages.push(message);
    }

    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    pub fn drain(&mut self) -> Vec<Value> {
        if self.mode == "all" {
            std::mem::take(&mut self.messages)
        } else {
            if self.messages.is_empty() {
                return Vec::new();
            }
            vec![self.messages.remove(0)]
        }
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

/// Port of the mutable agent state.
#[derive(Clone)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: Value,
    pub thinking_level: String,
    pub tools: Vec<Value>,
    pub messages: Vec<Value>,
    pub is_streaming: bool,
    pub streaming_message: Option<Value>,
    pub pending_tool_calls: Vec<String>,
    pub error_message: Option<String>,
}

impl AgentState {
    pub fn new(system_prompt: &str, model: Value, tools: Vec<Value>, messages: Vec<Value>) -> Self {
        AgentState {
            system_prompt: system_prompt.to_string(),
            model,
            thinking_level: "off".to_string(),
            tools,
            messages,
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: Vec::new(),
            error_message: None,
        }
    }
}

pub struct Agent {
    pub state: AgentState,
    steering_queue: PendingMessageQueue,
    follow_up_queue: PendingMessageQueue,
    listeners: Vec<Box<dyn FnMut(&Value)>>,
    pub session_id: Option<String>,
    /// Tool execution strategy: true mirrors alkaid's `toolExecution: "parallel"`.
    pub parallel_tools: bool,
    /// Optional mid-turn context-maintenance hook (Reasonix). Taken by
    /// `run_continuation` and passed into the loop config; `None` by default.
    pub prepare_next_turn: Option<Box<PrepareNextTurnFn<'static>>>,
    /// Optional shared steering queue for in-process control (native Vega):
    /// messages pushed here are drained into the loop between tool rounds,
    /// mirroring `steer`. `None` by default (parity tests unaffected).
    pub steer_queue:
        Option<std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Value>>>>,
}

impl Agent {
    pub fn new(state: AgentState, steering_mode: &str, follow_up_mode: &str) -> Self {
        Agent {
            state,
            steering_queue: PendingMessageQueue::new(steering_mode),
            follow_up_queue: PendingMessageQueue::new(follow_up_mode),
            listeners: Vec::new(),
            session_id: None,
            parallel_tools: false,
            prepare_next_turn: None,
            steer_queue: None,
        }
    }

    pub fn subscribe(&mut self, listener: Box<dyn FnMut(&Value)>) {
        self.listeners.push(listener);
    }

    pub fn steer(&mut self, message: Value) {
        self.steering_queue.enqueue(message);
    }

    pub fn follow_up(&mut self, message: Value) {
        self.follow_up_queue.enqueue(message);
    }

    pub fn clear_steering_queue(&mut self) {
        self.steering_queue.clear();
    }

    pub fn clear_follow_up_queue(&mut self) {
        self.follow_up_queue.clear();
    }

    pub fn has_queued_messages(&self) -> bool {
        self.steering_queue.has_items() || self.follow_up_queue.has_items()
    }

    /// Port of `normalizePromptInput`: a string becomes a single user message
    /// (with optional images); an array is used verbatim.
    pub fn normalize_prompt_input(&self, input: &Value, images: &[Value], timestamp: u64) -> Vec<Value> {
        if let Value::Array(items) = input {
            return items.clone();
        }
        if let Some(text) = input.as_str() {
            let mut content: Vec<Value> = vec![json!({ "type": "text", "text": text })];
            content.extend(images.iter().cloned());
            return vec![json!({ "role": "user", "content": content, "timestamp": timestamp })];
        }
        vec![input.clone()]
    }

    /// Reduce one loop event into agent state, then dispatch to listeners.
    /// Mirrors node `processEvents`.
    fn process_event(&mut self, event: &Value) {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") | Some("message_update") => {
                self.state.streaming_message = event.get("message").cloned();
            }
            Some("message_end") => {
                self.state.streaming_message = None;
                if let Some(message) = event.get("message") {
                    self.state.messages.push(message.clone());
                }
            }
            Some("tool_execution_start") => {
                if let Some(id) = event.get("toolCallId").and_then(Value::as_str) {
                    if !self.state.pending_tool_calls.iter().any(|x| x == id) {
                        self.state.pending_tool_calls.push(id.to_string());
                    }
                }
            }
            Some("tool_execution_end") => {
                if let Some(id) = event.get("toolCallId").and_then(Value::as_str) {
                    self.state.pending_tool_calls.retain(|x| x != id);
                }
            }
            Some("turn_end") => {
                if let Some(message) = event.get("message") {
                    if message.get("role").and_then(Value::as_str) == Some("assistant") {
                        if let Some(error) = message.get("errorMessage").and_then(Value::as_str) {
                            self.state.error_message = Some(error.to_string());
                        }
                    }
                }
            }
            Some("agent_end") => {
                self.state.streaming_message = None;
            }
            _ => {}
        }
        for listener in &mut self.listeners {
            listener(event);
        }
    }

    /// Port of `prompt`: normalize the input and run the loop. The steering and
    /// follow-up queues feed the loop config; `timestamp` stands in for
    /// `Date.now()`. Returns the events emitted during the run.
    pub fn prompt(
        &mut self,
        input: &Value,
        images: &[Value],
        timestamp: u64,
        stream_fn: &mut StreamFn,
        tool_fn: &mut ToolFn,
    ) -> Vec<Value> {
        let messages = self.normalize_prompt_input(input, images, timestamp);
        self.run_prompt_messages(&messages, false, timestamp, stream_fn, tool_fn)
    }

    /// Port of `continue`: resume from the current transcript. The last message
    /// must not be an assistant message (unless a queued message is drained).
    pub fn run_continuation(
        &mut self,
        timestamp: u64,
        stream_fn: &mut StreamFn,
        tool_fn: &mut ToolFn,
    ) -> Vec<Value> {
        // Continuation reuses the prompt path with no new messages; the loop's
        // initial steering poll and follow-up handling drive progress.
        self.run_prompt_messages(&[], false, timestamp, stream_fn, tool_fn)
    }

    fn run_prompt_messages(
        &mut self,
        messages: &[Value],
        _skip_initial_steering_poll: bool,
        timestamp: u64,
        stream_fn: &mut StreamFn,
        tool_fn: &mut ToolFn,
    ) -> Vec<Value> {
        self.state.is_streaming = true;
        self.state.streaming_message = None;
        self.state.error_message = None;

        // The loop works on a snapshot that includes prior history; the LLM
        // sees the full transcript while only new messages are emitted.
        let mut context = LoopContext {
            system_prompt: self.state.system_prompt.clone(),
            messages: self.state.messages.clone(),
            tools: self.state.tools.clone(),
        };

        let mut steering = PendingMessageQueue {
            messages: self.steering_queue.drain(),
            mode: self.steering_queue.mode.clone(),
        };
        let mut follow_up = PendingMessageQueue {
            messages: self.follow_up_queue.drain(),
            mode: self.follow_up_queue.mode.clone(),
        };

        // Steering drain: the pre-queued steering messages, plus (for native
        // Vega) any messages an external controller pushes to the shared queue.
        let steering_queue = self.steer_queue.clone();
        let steering_drain: Box<super::run_loop::QueueFn> = match steering_queue {
            Some(queue) => Box::new(move || {
                let mut messages = steering.drain();
                if let Ok(mut q) = queue.lock() {
                    while let Some(message) = q.pop_front() {
                        messages.push(message);
                    }
                }
                messages
            }),
            None => Box::new(move || steering.drain()),
        };

        let mut config = LoopConfig {
            get_steering_messages: Some(steering_drain),
            get_follow_up_messages: Some(Box::new(move || follow_up.drain())),
            prepare_next_turn: self.prepare_next_turn.take(),
            timestamp,
            parallel: self.parallel_tools,
        };

        let mut events: Vec<Value> = Vec::new();
        {
            let mut emit = |event: Value| events.push(event);
            run_agent_loop(
                messages,
                &mut context,
                &mut config,
                &mut emit,
                stream_fn,
                tool_fn,
            );
        }

        // Replay events to reduce state and dispatch listeners. Listeners are
        // side-effect-free for parity, so post-loop replay is equivalent to
        // node's in-loop `processEvents`.
        for event in &events {
            self.process_event(event);
        }

        self.state.is_streaming = false;
        self.state.streaming_message = None;
        self.state.pending_tool_calls.clear();
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_drain_all() {
        let mut queue = PendingMessageQueue::new("all");
        queue.enqueue(json!({ "a": 1 }));
        queue.enqueue(json!({ "b": 2 }));
        assert!(queue.has_items());
        let drained = queue.drain();
        assert_eq!(drained, vec![json!({ "a": 1 }), json!({ "b": 2 })]);
        assert!(!queue.has_items());
        assert!(queue.drain().is_empty());
    }

    #[test]
    fn queue_drain_one_at_a_time() {
        let mut queue = PendingMessageQueue::new("one-at-a-time");
        queue.enqueue(json!({ "a": 1 }));
        queue.enqueue(json!({ "b": 2 }));
        assert_eq!(queue.drain(), vec![json!({ "a": 1 })]);
        assert_eq!(queue.drain(), vec![json!({ "b": 2 })]);
        assert!(queue.drain().is_empty());
    }

    #[test]
    fn normalize_prompt_input_string() {
        let agent = Agent::new(
            AgentState::new("sys", json!({}), vec![], vec![]),
            "all",
            "one-at-a-time",
        );
        let messages = agent.normalize_prompt_input(&json!("hello"), &[], 123);
        assert_eq!(
            messages,
            vec![json!({ "role": "user", "content": [{ "type": "text", "text": "hello" }], "timestamp": 123 })]
        );
    }

    #[test]
    fn normalize_prompt_input_string_with_images() {
        let agent = Agent::new(
            AgentState::new("sys", json!({}), vec![], vec![]),
            "all",
            "one-at-a-time",
        );
        let image = json!({ "type": "image", "data": "x", "mimeType": "image/png" });
        let messages = agent.normalize_prompt_input(&json!("hi"), &[image.clone()], 1);
        assert_eq!(messages[0]["content"], json!([{ "type": "text", "text": "hi" }, image]));
    }

    #[test]
    fn normalize_prompt_input_array_passthrough() {
        let agent = Agent::new(
            AgentState::new("sys", json!({}), vec![], vec![]),
            "all",
            "one-at-a-time",
        );
        let input = json!([{ "role": "user", "content": [] }]);
        assert_eq!(agent.normalize_prompt_input(&input, &[], 1), input.as_array().unwrap().clone());
    }
}
