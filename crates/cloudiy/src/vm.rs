//! CloudiyOS VM manager — one persistent, identity-bound container per owner
//! identity on this provider, backed by a named Docker volume that survives
//! stop/start. Interactive shells attach via `docker exec -i`; published
//! ports are reachable through `cloudiy tunnel`.
//!
//! Deliberately Docker-specific, like the directory bootstrap: the `Runtime`
//! trait covers batch workloads, while the persistent-VM + interactive story
//! migrates behind a richer runtime seam once a second backend exists.
//!
//! Isolation: `--cap-drop ALL`, `--no-new-privileges`, a pids limit and the
//! spec's CPU/memory caps. A dev VM keeps network access (its purpose is to
//! install and build things) and published ports bind to 127.0.0.1 on the
//! provider so only the local tunnel — never the public interface — reaches
//! them.

use anyhow::{anyhow, Context, Result};
use cloudiy_common::VmInfo;
use cloudiy_protocol::{ResourceKind, ResourceVector, WorkloadSpec};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::process::Command;
use tracing::debug;

/// Default VM image when the spec names none — small, has a shell + apt.
pub const DEFAULT_IMAGE: &str = "debian:12-slim";

/// Ceiling on published ports per VM (each becomes a 127.0.0.1 bind).
const MAX_PORTS: usize = 16;

fn short(owner: &str) -> &str {
    &owner[..owner.len().min(16)]
}
fn container_name(owner: &str) -> String {
    format!("cloudiy-vm-{}", short(owner))
}
fn volume_name(owner: &str) -> String {
    format!("cloudiy-vol-{}", short(owner))
}

#[derive(Clone)]
struct VmRecord {
    vm_id: String,
    image: String,
    volume: String,
    ports: Vec<u16>,
    allocated: ResourceVector,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Default)]
pub struct VmManager {
    /// Keyed by owner identity (consumer EndpointId).
    vms: Mutex<HashMap<String, VmRecord>>,
    binary: String,
}

impl VmManager {
    pub fn new() -> Self {
        VmManager {
            vms: Mutex::new(HashMap::new()),
            binary: "docker".to_string(),
        }
    }

    async fn cli(&self, args: &[&str]) -> Result<std::process::Output> {
        debug!("{} {}", self.binary, args.join(" "));
        Ok(Command::new(&self.binary).args(args).output().await?)
    }

    fn record_to_info(&self, owner: &str, rec: &VmRecord, state: &str) -> VmInfo {
        VmInfo {
            vm_id: rec.vm_id.clone(),
            owner: owner.to_string(),
            image: rec.image.clone(),
            state: state.to_string(),
            cpu_millis: rec.allocated.get(&ResourceKind::Cpu),
            memory_mib: rec.allocated.get(&ResourceKind::Memory),
            gpu: rec.allocated.get(&ResourceKind::Gpu) > 0,
            volume: rec.volume.clone(),
            ports: rec.ports.clone(),
            created_at: rec.created_at,
        }
    }

    /// True when the owner already has a running VM.
    pub fn has_running(&self, owner: &str) -> bool {
        self.vms.lock().unwrap().contains_key(owner)
    }

    /// The resource vector a VM holds (for the caller to allocate before
    /// starting), derived from the spec with sane minimums.
    pub fn vm_resources(spec: &WorkloadSpec, gpu_available: bool) -> ResourceVector {
        let mut r = spec.resources.clone();
        if r.get(&ResourceKind::Cpu) == 0 {
            r = r.with(ResourceKind::Cpu, 1_000); // 1 core default
        }
        if r.get(&ResourceKind::Memory) == 0 {
            r = r.with(ResourceKind::Memory, 512); // 512 MiB default
        }
        if !gpu_available {
            r.0.remove(&ResourceKind::Gpu);
            r.0.remove(&ResourceKind::Vram);
        }
        r
    }

    /// Create the VM if absent, else return the existing one (idempotent).
    /// `allocated` is what the caller reserved in node accounting.
    pub async fn start(
        &self,
        owner: &str,
        spec: &WorkloadSpec,
        allocated: ResourceVector,
    ) -> Result<VmInfo> {
        if let Some(rec) = self.vms.lock().unwrap().get(owner).cloned() {
            return Ok(self.record_to_info(owner, &rec, "running"));
        }

        let image = spec.image.clone().unwrap_or_else(|| DEFAULT_IMAGE.to_string());
        let name = container_name(owner);
        let volume = volume_name(owner);
        let ports: Vec<u16> = spec.ports.iter().copied().take(MAX_PORTS).collect();

        // Named volume persists the VM home across stop/start.
        let out = self.cli(&["volume", "create", &volume]).await?;
        anyhow::ensure!(
            out.status.success(),
            "volume create failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Pull the image up front (clear error instead of a failed run).
        let out = self.cli(&["pull", &image]).await?;
        anyhow::ensure!(
            out.status.success(),
            "image pull failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let cpus = format!("{}", allocated.get(&ResourceKind::Cpu) as f64 / 1000.0);
        let mem = format!("{}m", allocated.get(&ResourceKind::Memory).max(64));
        let vol_mount = format!("{volume}:/root");
        let mut args: Vec<String> = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            name.clone(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--pids-limit".into(),
            "512".into(),
            "--cpus".into(),
            cpus,
            "--memory".into(),
            mem,
            "--volume".into(),
            vol_mount,
            "--workdir".into(),
            "/root".into(),
        ];
        if allocated.get(&ResourceKind::Gpu) > 0 {
            args.push("--gpus".into());
            args.push("all".into());
        }
        for p in &ports {
            // Bind to loopback only: reachable through `cloudiy tunnel`, never
            // the provider's public interface.
            args.push("--publish".into());
            args.push(format!("127.0.0.1:{p}:{p}"));
        }
        args.push(image.clone());
        // Keepalive so the container stays up for `docker exec` shells.
        args.push("sleep".into());
        args.push("infinity".into());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.cli(&arg_refs).await?;
        anyhow::ensure!(
            out.status.success(),
            "VM start failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let rec = VmRecord {
            vm_id: name,
            image,
            volume,
            ports,
            allocated,
            created_at: chrono::Utc::now(),
        };
        let info = self.record_to_info(owner, &rec, "running");
        self.vms.lock().unwrap().insert(owner.to_string(), rec);
        Ok(info)
    }

    pub fn status(&self, owner: &str) -> Option<VmInfo> {
        self.vms
            .lock()
            .unwrap()
            .get(owner)
            .map(|rec| self.record_to_info(owner, rec, "running"))
    }

    /// Destroy the VM. Returns the resource vector to release (empty if the
    /// owner had no VM). `wipe` also deletes the persistent volume.
    pub async fn stop(&self, owner: &str, wipe: bool) -> Result<ResourceVector> {
        let Some(rec) = self.vms.lock().unwrap().remove(owner) else {
            return Ok(ResourceVector::new());
        };
        // Idempotent removal of the container.
        self.cli(&["rm", "--force", &rec.vm_id]).await.ok();
        if wipe {
            self.cli(&["volume", "rm", "--force", &rec.volume]).await.ok();
        }
        Ok(rec.allocated)
    }

    /// Spawn an interactive shell inside the VM. Caller pumps bytes between
    /// the QUIC session stream and the child's piped stdio.
    pub async fn open_shell(
        &self,
        owner: &str,
        command: &[String],
    ) -> Result<tokio::process::Child> {
        let name = {
            let vms = self.vms.lock().unwrap();
            vms.get(owner)
                .map(|r| r.vm_id.clone())
                .ok_or_else(|| anyhow!("no VM for this identity — run `cloudiy vm up` first"))?
        };

        let mut cmd = Command::new(&self.binary);
        cmd.arg("exec").arg("-i").arg(&name);
        if command.is_empty() {
            cmd.arg("/bin/sh");
        } else {
            cmd.args(command);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd.spawn().context("docker exec failed to spawn")
    }
}
