//! Docker driver — today's implementation, never the protocol.
//!
//! Uses the `docker` CLI (no daemon socket dependency in the SDK sense) with
//! a hardened default profile: no new privileges, all capabilities dropped,
//! read-only root filesystem when the workload needs no persistent storage,
//! resource limits from the spec, and no network unless ports are exposed.

use crate::{ExecutionHandle, MetricsSnapshot, Outcome, Runtime};
use cloudiy_protocol::{ResourceKind, WorkloadSpec};
use tokio::process::Command;
use tracing::debug;

pub struct DockerRuntime {
    /// Binary to invoke — override for podman compatibility (`podman` speaks
    /// the same CLI), proving the driver is not welded to Docker Inc.
    pub binary: String,
    /// OCI runtime to run containers under (`--runtime`): `runc` (default),
    /// `runsc` (gVisor) or `kata-runtime` (Kata Containers) for microVM-class
    /// isolation of untrusted workloads. `None` = Docker's default (runc).
    pub runtime: Option<String>,
}

impl Default for DockerRuntime {
    fn default() -> Self {
        DockerRuntime {
            binary: "docker".to_string(),
            runtime: None,
        }
    }
}

impl DockerRuntime {
    pub fn with_runtime(runtime: Option<String>) -> Self {
        DockerRuntime {
            binary: "docker".to_string(),
            runtime,
        }
    }
}

impl DockerRuntime {
    fn image_of(spec: &WorkloadSpec) -> anyhow::Result<&str> {
        spec.image
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("workload has no image (template expansion pending)"))
    }

    async fn cli(&self, args: &[&str]) -> anyhow::Result<std::process::Output> {
        debug!("{} {}", self.binary, args.join(" "));
        Ok(Command::new(&self.binary).args(args).output().await?)
    }
}

#[async_trait::async_trait]
impl Runtime for DockerRuntime {
    fn name(&self) -> &'static str {
        "docker"
    }

    async fn supports(&self) -> bool {
        self.cli(&["version", "--format", "{{.Server.Version}}"])
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn prepare(&self, spec: &WorkloadSpec) -> anyhow::Result<()> {
        let image = Self::image_of(spec)?;
        let out = self.cli(&["pull", image]).await?;
        anyhow::ensure!(
            out.status.success(),
            "image pull failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }

    async fn run(&self, workload_id: &str, spec: &WorkloadSpec) -> anyhow::Result<ExecutionHandle> {
        let image = Self::image_of(spec)?.to_string();
        let name = format!("cloudiy-{workload_id}");

        let mut args: Vec<String> = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            name.clone(),
            // Isolation is mandatory: never on the host.
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--pids-limit".into(),
            "512".into(),
        ];
        // Stronger-than-container isolation when a sandboxed OCI runtime is
        // selected (gVisor `runsc` / Kata) — the seam the protocol promises
        // for untrusted workloads on a public network.
        if let Some(rt) = &self.runtime {
            args.push("--runtime".into());
            args.push(rt.clone());
        }

        // Read-only rootfs whenever possible (no persistent storage asked).
        if spec.persistent_storage_mib == 0 {
            args.push("--read-only".into());
            args.push("--tmpfs".into());
            args.push("/tmp".into());
        }

        // Resource limits from the spec (cpu millicores → --cpus).
        let cpu = spec.resources.get(&ResourceKind::Cpu);
        if cpu > 0 {
            args.push("--cpus".into());
            args.push(format!("{}", cpu as f64 / 1000.0));
        }
        let mem = spec.resources.get(&ResourceKind::Memory);
        if mem > 0 {
            args.push("--memory".into());
            args.push(format!("{mem}m"));
        }
        if spec.resources.get(&ResourceKind::Gpu) > 0 || spec.resources.get(&ResourceKind::Vram) > 0
        {
            args.push("--gpus".into());
            args.push("all".into());
        }

        // Network only when the workload exposes ports.
        if spec.ports.is_empty() {
            args.push("--network".into());
            args.push("none".into());
        } else {
            for p in &spec.ports {
                args.push("--publish".into());
                args.push(format!("{p}:{p}"));
            }
        }

        for (k, v) in &spec.env {
            args.push("--env".into());
            args.push(format!("{k}={v}"));
        }

        args.push(image);
        args.extend(spec.command.iter().cloned());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.cli(&arg_refs).await?;
        anyhow::ensure!(
            out.status.success(),
            "container start failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(ExecutionHandle(name))
    }

    async fn wait(&self, handle: &ExecutionHandle) -> anyhow::Result<Outcome> {
        let out = self.cli(&["wait", &handle.0]).await?;
        anyhow::ensure!(
            out.status.success(),
            "wait failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let code: i32 = String::from_utf8_lossy(&out.stdout).trim().parse()?;
        Ok(if code == 0 {
            Outcome::Succeeded
        } else {
            Outcome::Failed {
                exit_code: Some(code),
            }
        })
    }

    async fn logs(&self, handle: &ExecutionHandle) -> anyhow::Result<String> {
        let out = self.cli(&["logs", &handle.0]).await?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(text)
    }

    async fn metrics(&self, handle: &ExecutionHandle) -> anyhow::Result<MetricsSnapshot> {
        let out = self
            .cli(&["stats", "--no-stream", "--format", "{{json .}}", &handle.0])
            .await?;
        let raw: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
                .unwrap_or(serde_json::Value::Null);
        let cpu_percent = raw["CPUPerc"]
            .as_str()
            .and_then(|s| s.trim_end_matches('%').parse().ok());
        Ok(MetricsSnapshot {
            cpu_percent,
            memory_mib: None,
            raw,
        })
    }

    async fn destroy(&self, handle: ExecutionHandle) -> anyhow::Result<()> {
        // Idempotent: forcing removal of a gone container is fine.
        self.cli(&["rm", "--force", &handle.0]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute;
    use cloudiy_protocol::ResourceVector;

    /// Full lifecycle against real Docker. Self-skips when Docker is absent
    /// so CI without a daemon stays green.
    #[tokio::test]
    async fn docker_lifecycle_echo() {
        let rt = DockerRuntime::default();
        if !rt.supports().await {
            eprintln!("docker not available — skipping integration test");
            return;
        }
        let spec = WorkloadSpec {
            image: Some("alpine:3.20".into()),
            command: vec!["echo".into(), "hello-cloudiy".into()],
            resources: ResourceVector::new(),
            ..Default::default()
        };
        let (outcome, logs) = execute(&rt, "test-echo", &spec).await.unwrap();
        assert_eq!(outcome, Outcome::Succeeded);
        assert!(logs.contains("hello-cloudiy"));
    }
}
