//! In-memory runtime for unit tests and for proving the abstraction holds:
//! if the scheduler/API work against [`MockRuntime`], they work against any
//! future runtime (Firecracker, Kata, WASM…).

use crate::{ExecutionHandle, MetricsSnapshot, Outcome, Runtime};
use cloudiy_protocol::WorkloadSpec;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct MockRuntime {
    executions: Mutex<HashMap<String, (WorkloadSpec, bool)>>,
}

impl MockRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn destroyed(&self, handle: &str) -> bool {
        self.executions
            .lock()
            .unwrap()
            .get(handle)
            .map(|(_, destroyed)| *destroyed)
            .unwrap_or(false)
    }
}

#[async_trait::async_trait]
impl Runtime for MockRuntime {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn supports(&self) -> bool {
        true
    }

    async fn prepare(&self, _spec: &WorkloadSpec) -> anyhow::Result<()> {
        Ok(())
    }

    async fn run(
        &self,
        workload_id: &str,
        spec: &WorkloadSpec,
    ) -> anyhow::Result<ExecutionHandle> {
        self.executions
            .lock()
            .unwrap()
            .insert(workload_id.to_string(), (spec.clone(), false));
        Ok(ExecutionHandle(workload_id.to_string()))
    }

    async fn wait(&self, handle: &ExecutionHandle) -> anyhow::Result<Outcome> {
        let map = self.executions.lock().unwrap();
        let (spec, _) = map
            .get(&handle.0)
            .ok_or_else(|| anyhow::anyhow!("unknown handle"))?;
        // Convention for tests: command ["fail"] simulates a failure.
        Ok(if spec.command.first().map(String::as_str) == Some("fail") {
            Outcome::Failed { exit_code: Some(1) }
        } else {
            Outcome::Succeeded
        })
    }

    async fn logs(&self, handle: &ExecutionHandle) -> anyhow::Result<String> {
        Ok(format!("mock logs for {}", handle.0))
    }

    async fn metrics(&self, _handle: &ExecutionHandle) -> anyhow::Result<MetricsSnapshot> {
        Ok(MetricsSnapshot::default())
    }

    async fn destroy(&self, handle: ExecutionHandle) -> anyhow::Result<()> {
        if let Some(entry) = self.executions.lock().unwrap().get_mut(&handle.0) {
            entry.1 = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute;

    #[tokio::test]
    async fn lifecycle_always_destroys_the_environment() {
        let rt = MockRuntime::new();
        let spec = WorkloadSpec {
            command: vec!["echo".into()],
            ..Default::default()
        };
        let (outcome, logs) = execute(&rt, "w1", &spec).await.unwrap();
        assert_eq!(outcome, Outcome::Succeeded);
        assert!(logs.contains("w1"));
        assert!(rt.destroyed("w1"), "environment must be destroyed");
    }

    #[tokio::test]
    async fn failure_still_destroys_the_environment() {
        let rt = MockRuntime::new();
        let spec = WorkloadSpec {
            command: vec!["fail".into()],
            ..Default::default()
        };
        let (outcome, _) = execute(&rt, "w2", &spec).await.unwrap();
        assert_eq!(outcome, Outcome::Failed { exit_code: Some(1) });
        assert!(rt.destroyed("w2"), "even failed workloads release resources");
    }
}
