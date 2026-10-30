//! Butterfly follows employee tasks, not the turns used to execute their steps.

use super::{conclusion, Kind};
use crate::{employee::Employee, threads::Thread};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Task {
    pub id: String,
    pub thread: String,
    pub current: String,
    pub title: String,
    pub created_at: i64,
    pub kind: Kind,
    pub text: String,
    pub body: String,
}

impl Task {
    pub(super) fn active(&self) -> bool { self.kind == Kind::Waiting }
}

fn project(employee: &Employee) -> Vec<Task> {
    employee.inbox.iter().filter(|t| !t.confirm).map(|t| (t, true))
        .chain(employee.finished.iter().filter(|t| !t.confirm && matches!(t.status.as_str(), "done" | "failed")).map(|t| (t, false)))
        .filter_map(|(todo, active)| {
            let current = todo.thread_id.as_ref().or(todo.work_thread.as_ref()).or(todo.computer_thread.as_ref())?;
            let thread = if active { todo.work_thread.as_ref().unwrap_or(current) } else { current };
            let (kind, text, body) = if active {
                let text = if !todo.approval.is_empty() { "等待批准" }
                    else if !todo.waiting.is_empty() { "等待补充信息" }
                    else if !todo.checkpoint.is_empty() { "等待外部结果" }
                    else if todo.desktop { "等待电脑续做" }
                    else { "等待续做" };
                let body = if !todo.waiting.is_empty() { todo.waiting.clone() }
                    else { todo.progress.last().map(|p| p.text.clone()).unwrap_or_default() };
                (Kind::Waiting, text.to_string(), body)
            } else {
                let kind = if todo.status == "failed" { Kind::Error } else { Kind::Done };
                let text = conclusion(&todo.result).unwrap_or_else(|| if kind == Kind::Error { "任务出错" } else { "已完成" }.into());
                (kind, text, todo.result.clone())
            };
            Some(Task { id: todo.id.clone(), thread: thread.clone(), current: current.clone(),
                title: format!("员工 · {}", conclusion(&todo.text).unwrap_or_else(|| "任务".into())),
                created_at: todo.created_at, kind, text, body })
        }).collect()
}

#[cfg(windows)]
static TASKS: std::sync::Mutex<Option<std::sync::Arc<[Task]>>> = std::sync::Mutex::new(None);

#[cfg(windows)]
pub(super) fn changed(employee: &Employee) {
    *TASKS.lock().unwrap() = Some(project(employee).into());
}

#[cfg(windows)]
pub(super) fn snapshot() -> std::sync::Arc<[Task]> { TASKS.lock().unwrap().clone().unwrap_or_default() }

/// Raw employee turns never compete with their task, including during handoff gaps.
pub(super) fn entries<'a>(threads: &'a [Thread], tasks: &'a [Task]) -> impl Iterator<Item = (&'a Thread, Option<&'a Task>)> {
    let mut employee_threads: std::collections::HashSet<&str> = threads.iter().filter(|t| t.employee_thread).map(|t| t.id.as_str()).collect();
    employee_threads.extend(tasks.iter().flat_map(|t| [t.thread.as_str(), t.current.as_str()]));
    loop {
        let before = employee_threads.len();
        for thread in threads {
            if thread.parent_thread_id.as_deref().is_some_and(|id| employee_threads.contains(id)) {
                employee_threads.insert(&thread.id);
            }
        }
        if employee_threads.len() == before { break; }
    }
    threads.iter().filter(move |t| !employee_threads.contains(t.id.as_str())).map(|t| (t, None))
        .chain(tasks.iter().filter_map(move |task| {
            let thread = threads.iter().find(|t| t.id == task.current).or_else(|| threads.iter().find(|t| t.id == task.thread))?;
            Some((thread, Some(task)))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{employee::{Progress, Todo}, threads::AgentKind};

    fn thread(id: &str, employee: bool) -> Thread {
        let mut t = Thread::new(String::new(), AgentKind::Lyra, None, None, None, false);
        t.id = id.into();
        t.employee_thread = employee;
        t
    }

    #[test]
    fn handoffs_and_pauses_keep_one_task_without_intermediate_results() {
        let mut threads = vec![thread("work", true), thread("computer", true), thread("old-step", true), thread("chat", false)];
        // Children need not inherit the employee flag; resolve ancestry, even in reverse order.
        let mut nested = thread("nested", false);
        nested.parent_thread_id = Some("child".into());
        let mut child = thread("child", false);
        child.parent_thread_id = Some("computer".into());
        threads.extend([nested, child]);
        let mut e = Employee::default();
        e.inbox.push(Todo { id: "task".into(), text: "完整任务".into(), created_at: 10,
            thread_id: Some("work".into()), work_thread: Some("work".into()), computer_thread: Some("computer".into()),
            progress: vec![Progress { at: 20, text: "正在验证".into() }], ..Default::default() });
        for current in ["work", "computer", "work"] {
            e.inbox[0].thread_id = Some(current.into());
            let tasks = project(&e);
            let entries: Vec<_> = entries(&threads, &tasks).collect();
            assert_eq!(entries.len(), 2, "one ordinary conversation and one employee task, never per-step entries");
            let task = entries[1].1.unwrap();
            assert_eq!((task.id.as_str(), task.thread.as_str(), task.current.as_str()), ("task", "work", current));
            assert!(task.active());
            assert_eq!(task.created_at, 10, "handoffs do not reset the task timer");
            assert_eq!(task.body, "正在验证", "a step's reply is not a task result");
        }
        for (waiting, approval, checkpoint, desktop, expected) in [
            ("需要验证码", "", "", false, "等待补充信息"),
            ("", "confirmation", "", false, "等待批准"),
            ("", "", "build:123", false, "等待外部结果"),
            ("", "", "", true, "等待电脑续做"),
        ] {
            let todo = &mut e.inbox[0];
            todo.waiting = waiting.into(); todo.approval = approval.into(); todo.checkpoint = checkpoint.into(); todo.desktop = desktop;
            let tasks = project(&e);
            assert!(tasks[0].active());
            assert_eq!(tasks[0].text, expected);
        }
    }

    #[test]
    fn only_archived_task_results_are_terminal_and_independent_tasks_stay_separate() {
        let mut e = Employee::default();
        e.finished.push(Todo { id: "done".into(), text: "完整任务".into(), status: "done".into(),
            thread_id: Some("work".into()), work_thread: Some("work".into()), result: "最终结论\n全部验证通过".into(), ..Default::default() });
        e.inbox.push(Todo { id: "derived".into(), origin: "done".into(), duty_id: "duty".into(),
            thread_id: Some("other".into()), ..Default::default() });
        e.inbox.push(Todo { id: "confirm".into(), confirm: true, thread_id: Some("other".into()), ..Default::default() });
        let tasks = project(&e);
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].active(), "derived tasks are independent, not merged by origin or duty");
        assert_eq!((tasks[1].kind, tasks[1].text.as_str(), tasks[1].body.as_str()), (Kind::Done, "最终结论", "最终结论\n全部验证通过"));
        e.finished[0].status = "failed".into();
        e.finished[0].thread_id = Some("computer".into());
        let task = project(&e).pop().unwrap();
        assert_eq!(task.kind, Kind::Error);
        assert_eq!(task.thread, "computer", "a terminal computer failure opens its actual result");
        e.finished[0].status = "stopped".into();
        assert_eq!(project(&e).len(), 1, "manual stops do not announce completion");
    }

    #[test]
    fn unread_step_results_never_resurface_after_the_task_is_seen() {
        let mut threads = [thread("work", true), thread("computer", true), thread("old-step", true)];
        for (n, thread) in threads.iter_mut().enumerate() {
            thread.updated_at = 100 + n as i64;
            thread.unread_turns = 1;
        }
        let mut e = Employee::default();
        e.inbox.push(Todo { id: "task".into(), thread_id: Some("work".into()), ..Default::default() });
        let pick = |e: &Employee, threads: &[Thread]| {
            let tasks = project(e);
            let candidates: Vec<_> = entries(threads, &tasks).filter(|(_, task)| !task.is_some_and(|t| t.active()))
                .map(|(t, _)| (t.id.as_str(), t.updated_at, t.unread_turns, t.taskbar_seen_at)).collect();
            super::super::pick(&candidates, None).map(str::to_string)
        };
        assert_eq!(pick(&e, &threads), None, "a completed turn is not a completed task");
        let mut done = e.inbox.remove(0);
        done.status = "done".into(); done.result = "最终结果".into();
        e.finished.push(done);
        assert_eq!(pick(&e, &threads).as_deref(), Some("work"));
        threads[0].taskbar_seen_at = Some(threads[0].updated_at);
        assert_eq!(pick(&e, &threads), None, "newer unread computer/old-step turns remain hidden after final acknowledgement");
    }
}
