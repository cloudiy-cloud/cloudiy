//! CloudiyOS VM manager — one persistent container per owner identity on this
//! provider. Its home is a Docker volume. With `CLOUDIY_VOLUME_REMOTE` set the
//! authoritative state lives OFF the provider (an rclone remote): it is
//! restored into a transient working volume on start and synced back on stop,
//! so the same identity can resume on any node and no durable state is left on
//! the provider. Without it, the volume is a provider-local persistent home.
//! Interactive shells attach via `docker exec -i`; published ports are
//! reachable through `cloudiy tunnel`.
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

/// Reject a docker image reference that isn't a plain name. Docker stops
/// flag-parsing at the first positional, but a *token that begins with `-`*
/// is still consumed as a flag — so `image = "--privileged"` would inject a
/// run flag and defeat our isolation choices. Constrain to the characters a
/// real image reference uses and forbid a leading dash.
fn validate_image(image: &str) -> Result<()> {
    anyhow::ensure!(!image.is_empty(), "image must not be empty");
    anyhow::ensure!(image.len() <= 256, "image reference too long");
    anyhow::ensure!(
        !image.starts_with('-'),
        "invalid image reference (leading '-' would be read as a docker flag)"
    );
    anyhow::ensure!(
        image
            .bytes()
            .all(|b| b.is_ascii_alphanumeric()
                || matches!(b, b'_' | b'.' | b'/' | b':' | b'@' | b'-')),
        "invalid image reference: only [A-Za-z0-9_./:@-] are allowed"
    );
    Ok(())
}

/// Same guard for `docker exec` command tokens: no argument may begin with a
/// dash, or it would be parsed as an exec flag rather than shell input.
fn validate_command(command: &[String]) -> Result<()> {
    for arg in command {
        anyhow::ensure!(
            !arg.starts_with('-'),
            "invalid shell argument '{arg}': leading '-' would be read as a docker exec flag"
        );
    }
    Ok(())
}

fn short(owner: &str) -> &str {
    &owner[..owner.len().min(16)]
}
fn container_name(owner: &str) -> String {
    format!("cloudiy-vm-{}", short(owner))
}
fn volume_name(owner: &str) -> String {
    format!("cloudiy-vol-{}", short(owner))
}

/// External, off-provider durable store for VM volumes (an rclone remote path,
/// e.g. `s3:cloudiy-vms` or `r2:vms`). When set, a VM's authoritative state
/// lives HERE, not on the provider: it is restored into a transient working
/// volume on start and synced back on stop, so the same identity can be
/// restored on any node and no durable state is left on the provider. Unset =
/// legacy behaviour (a provider-local named volume).
fn volume_remote() -> Option<String> {
    std::env::var("CLOUDIY_VOLUME_REMOTE")
        .ok()
        .filter(|v| !v.is_empty())
}

#[derive(Clone)]
struct VmRecord {
    vm_id: String,
    image: String,
    volume: String,
    ports: Vec<u16>,
    allocated: ResourceVector,
    created_at: chrono::DateTime<chrono::Utc>,
    /// Prepaid compute lease. `rate` is the node's hourly price; `budget` is
    /// the funded escrow amount. `budget == 0` means unmetered (dev mode).
    rate_micro_usdc_per_hour: u64,
    budget_micro_usdc: u64,
}

impl VmRecord {
    /// micro-USDC accrued so far: uptime × hourly rate (u128 to avoid overflow).
    fn accrued(&self, now: chrono::DateTime<chrono::Utc>) -> u64 {
        let secs = (now - self.created_at).num_seconds().max(0) as u128;
        ((secs * self.rate_micro_usdc_per_hour as u128) / 3600) as u64
    }

    /// Seconds of lease remaining; `None` when unmetered.
    fn remaining_secs(&self, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
        if self.budget_micro_usdc == 0 || self.rate_micro_usdc_per_hour == 0 {
            return None;
        }
        let left = self.budget_micro_usdc.saturating_sub(self.accrued(now));
        Some((left as i128 * 3600 / self.rate_micro_usdc_per_hour as i128) as i64)
    }

    /// True once a metered lease is fully spent.
    fn exhausted(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.budget_micro_usdc > 0
            && self.rate_micro_usdc_per_hour > 0
            && self.accrued(now) >= self.budget_micro_usdc
    }
}

#[derive(Default)]
pub struct VmManager {
    /// Keyed by owner identity (consumer EndpointId).
    vms: Mutex<HashMap<String, VmRecord>>,
    binary: String,
    /// OCI runtime (`--runtime`) for microVM-class isolation; None = runc.
    runtime: Option<String>,
}

impl VmManager {
    pub fn new(runtime: Option<String>) -> Self {
        VmManager {
            vms: Mutex::new(HashMap::new()),
            binary: "docker".to_string(),
            runtime,
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
            price_micro_usdc_per_hour: rec.rate_micro_usdc_per_hour,
            lease_micro_usdc: rec.budget_micro_usdc,
            lease_remaining_secs: rec.remaining_secs(chrono::Utc::now()),
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
        rate_micro_usdc_per_hour: u64,
        budget_micro_usdc: u64,
    ) -> Result<VmInfo> {
        if let Some(rec) = self.vms.lock().unwrap().get(owner).cloned() {
            return Ok(self.record_to_info(owner, &rec, "running"));
        }

        let image = spec
            .image
            .clone()
            .unwrap_or_else(|| DEFAULT_IMAGE.to_string());
        validate_image(&image)?;
        let name = container_name(owner);
        let volume = volume_name(owner);
        let ports: Vec<u16> = spec.ports.iter().copied().take(MAX_PORTS).collect();

        // A working volume for the VM home. With an external store configured
        // it is transient (created here, synced from the store below, and
        // dropped on stop); otherwise it is the provider-local persistent home.
        let out = self.cli(&["volume", "create", &volume]).await?;
        anyhow::ensure!(
            out.status.success(),
            "volume create failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Restore this identity's state from the off-provider store into the
        // working volume before the VM runs. Best-effort: a brand-new VM
        // legitimately has no prior state, so a failed/empty restore must not
        // block startup (persist on stop is the strict half).
        if volume_remote().is_some() {
            if let Err(e) = self.volume_sync(owner, &volume, false).await {
                tracing::warn!("external volume restore skipped for {}: {e}", short(owner));
            }
        }

        // Pull the image up front (clear error instead of a failed run).
        let out = self.cli(&["pull", &image]).await?;
        anyhow::ensure!(
            out.status.success(),
            "image pull failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Defensive: if an untracked container with this name lingers (e.g.
        // from an older version without labels that reconcile couldn't adopt),
        // remove it so the run below can't collide. We only reach here when
        // the owner has no tracked VM, so any same-named container is stale.
        self.cli(&["rm", "--force", &name]).await.ok();

        let cpu_millis = allocated.get(&ResourceKind::Cpu);
        let mem_mib = allocated.get(&ResourceKind::Memory);
        let gpu_flag = if allocated.get(&ResourceKind::Gpu) > 0 {
            "1"
        } else {
            "0"
        };
        let ports_csv = ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let cpus = format!("{}", cpu_millis as f64 / 1000.0);
        let mem = format!("{}m", mem_mib.max(64));
        let vol_mount = format!("{volume}:/root");
        // A dev VM must behave like a real machine (apt/pip/npm install), so
        // we keep Docker's default capability set (already excludes SYS_ADMIN,
        // NET_ADMIN, etc.) rather than `--cap-drop ALL`, which breaks apt's
        // privilege-dropping http method. `no-new-privileges` still blocks
        // setuid escalation and a pids limit bounds fork bombs.
        let mut args: Vec<String> = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            name.clone(),
        ];
        // Sandboxed OCI runtime (gVisor/Kata) for stronger isolation of an
        // untrusted tenant's VM, when the provider selected one.
        if let Some(rt) = &self.runtime {
            args.push("--runtime".into());
            args.push(rt.clone());
        }
        args.extend([
            // Labels let a restarted provider rebuild its VM map from Docker
            // (see `reconcile`) — the full owner id lives here, not just the
            // 16-char prefix in the container name.
            "--label".into(),
            "cloudiy.managed=1".into(),
            "--label".into(),
            format!("cloudiy.owner={owner}"),
            "--label".into(),
            format!("cloudiy.image={image}"),
            "--label".into(),
            format!("cloudiy.cpu={cpu_millis}"),
            "--label".into(),
            format!("cloudiy.mem={mem_mib}"),
            "--label".into(),
            format!("cloudiy.gpu={gpu_flag}"),
            "--label".into(),
            format!("cloudiy.ports={ports_csv}"),
            "--label".into(),
            format!("cloudiy.volume={volume}"),
            // Lease labels so a restarted provider keeps metering the VM from
            // its original start time (restart doesn't extend the lease).
            "--label".into(),
            format!("cloudiy.rate={rate_micro_usdc_per_hour}"),
            "--label".into(),
            format!("cloudiy.budget={budget_micro_usdc}"),
            "--label".into(),
            format!("cloudiy.created={}", chrono::Utc::now().to_rfc3339()),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--pids-limit".into(),
            "1024".into(),
            "--cpus".into(),
            cpus,
            "--memory".into(),
            mem,
            "--volume".into(),
            vol_mount,
            "--workdir".into(),
            "/root".into(),
        ]);
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
            rate_micro_usdc_per_hour,
            budget_micro_usdc,
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

    /// True when this owner's metered lease is fully spent — used to refuse new
    /// shell/tunnel opens against a VM the reaper is about to (or already did)
    /// stop.
    pub fn lease_exhausted(&self, owner: &str) -> bool {
        let now = chrono::Utc::now();
        self.vms
            .lock()
            .unwrap()
            .get(owner)
            .map(|rec| rec.exhausted(now))
            .unwrap_or(false)
    }

    /// Stop every VM whose prepaid lease is spent and return their owners.
    /// Released resources go back into node accounting. Runs on a timer (see
    /// the reaper spawned in `main`), so a tenant can't hold hardware past what
    /// they paid for (#2). Unmetered (dev) VMs are never reaped.
    pub async fn reap_expired(
        &self,
        resources: &Mutex<cloudiy_protocol::Resources>,
    ) -> Vec<String> {
        let now = chrono::Utc::now();
        let expired: Vec<String> = self
            .vms
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, rec)| rec.exhausted(now))
            .map(|(owner, _)| owner.clone())
            .collect();
        for owner in &expired {
            if let Ok(released) = self.stop(owner, false).await {
                resources.lock().unwrap().release(&released);
            }
        }
        expired
    }

    /// Destroy the VM. Returns the resource vector to release (empty if the
    /// Sync a VM's working volume with the external durable store via a
    /// transient rclone container. `to_remote` persists (working → store);
    /// otherwise it restores (store → working). No-op when no remote is
    /// configured. The store path is namespaced by the full owner id.
    async fn volume_sync(&self, owner: &str, volume: &str, to_remote: bool) -> Result<()> {
        let Some(remote) = volume_remote() else {
            return Ok(());
        };
        let remote_path = format!("{}/{}", remote.trim_end_matches('/'), owner);
        let vol_mount = format!("{volume}:/data");
        let (src, dst) = if to_remote {
            ("/data".to_string(), remote_path)
        } else {
            (remote_path, "/data".to_string())
        };
        let mut args: Vec<String> = vec!["run".into(), "--rm".into(), "-v".into(), vol_mount];
        // Mount the operator's rclone config (remotes + credentials) read-only.
        if let Ok(cfg) = std::env::var("CLOUDIY_RCLONE_CONFIG") {
            args.push("-v".into());
            args.push(format!("{cfg}:/config/rclone/rclone.conf:ro"));
        }
        args.extend([
            "rclone/rclone".into(),
            "copy".into(),
            src,
            dst,
            "--transfers".into(),
            "8".into(),
        ]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.cli(&refs).await?;
        anyhow::ensure!(
            out.status.success(),
            "external volume sync failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }

    /// owner had no VM). `wipe` also deletes the persistent volume.
    pub async fn stop(&self, owner: &str, wipe: bool) -> Result<ResourceVector> {
        let Some(rec) = self.vms.lock().unwrap().remove(owner) else {
            return Ok(ResourceVector::new());
        };
        // Idempotent removal of the container.
        self.cli(&["rm", "--force", &rec.vm_id]).await.ok();
        if wipe {
            self.cli(&["volume", "rm", "--force", &rec.volume])
                .await
                .ok();
        } else if volume_remote().is_some() {
            // Persist to the off-provider store, THEN drop the local working
            // copy so no durable state is left on the provider. Only remove the
            // local volume once the external persist succeeded — otherwise keep
            // it as a fallback rather than lose data.
            match self.volume_sync(owner, &rec.volume, true).await {
                Ok(()) => {
                    self.cli(&["volume", "rm", "--force", &rec.volume])
                        .await
                        .ok();
                }
                Err(e) => tracing::error!(
                    "external volume persist failed for {} — keeping local copy: {e}",
                    short(owner)
                ),
            }
        }
        Ok(rec.allocated)
    }

    /// Rebuild in-memory VM state from Docker after a provider restart, and
    /// re-reserve each VM's resources in node accounting. Managed containers
    /// that are stopped get restarted; ones that can't be revived are removed.
    /// Prevents orphaned containers and `vm up` name collisions across
    /// restarts. Returns the number of VMs adopted.
    pub async fn reconcile(&self, resources: &Mutex<cloudiy_protocol::Resources>) -> usize {
        const FMT: &str = "{{.Names}}\t{{.State}}\t{{.Label \"cloudiy.owner\"}}\t{{.Label \"cloudiy.image\"}}\t{{.Label \"cloudiy.cpu\"}}\t{{.Label \"cloudiy.mem\"}}\t{{.Label \"cloudiy.gpu\"}}\t{{.Label \"cloudiy.ports\"}}\t{{.Label \"cloudiy.volume\"}}\t{{.Label \"cloudiy.rate\"}}\t{{.Label \"cloudiy.budget\"}}\t{{.Label \"cloudiy.created\"}}";
        let out = match self
            .cli(&[
                "ps",
                "-a",
                "--filter",
                "label=cloudiy.managed",
                "--format",
                FMT,
            ])
            .await
        {
            Ok(o) if o.status.success() => o,
            _ => return 0,
        };
        let text = String::from_utf8_lossy(&out.stdout).into_owned();

        let mut adopted = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 9 || f[2].is_empty() {
                continue;
            }
            let (name, state, owner, image) = (f[0], f[1], f[2].to_string(), f[3]);

            // Bring a stopped VM back up; drop it if it won't start.
            if !state.eq_ignore_ascii_case("running") {
                let started = self
                    .cli(&["start", name])
                    .await
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !started {
                    self.cli(&["rm", "--force", name]).await.ok();
                    continue;
                }
            }

            let mut allocated = ResourceVector::new();
            if let Ok(v) = f[4].parse::<u64>() {
                if v > 0 {
                    allocated = allocated.with(ResourceKind::Cpu, v);
                }
            }
            if let Ok(v) = f[5].parse::<u64>() {
                if v > 0 {
                    allocated = allocated.with(ResourceKind::Memory, v);
                }
            }
            if f[6] == "1" {
                allocated = allocated.with(ResourceKind::Gpu, 1);
            }
            let ports = f[7].split(',').filter_map(|p| p.parse().ok()).collect();
            let volume = if f[8].is_empty() {
                volume_name(&owner)
            } else {
                f[8].to_string()
            };
            // Lease labels (added later — tolerate their absence on old VMs).
            let rate_micro_usdc_per_hour = f.get(9).and_then(|s| s.parse().ok()).unwrap_or(0);
            let budget_micro_usdc = f.get(10).and_then(|s| s.parse().ok()).unwrap_or(0);
            let created_at = f
                .get(11)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(chrono::Utc::now);

            // Best-effort re-reservation; if the machine now shares less than
            // before, allocation may fail — the VM still runs, just uncounted.
            resources.lock().unwrap().allocate(&allocated).ok();

            let rec = VmRecord {
                vm_id: name.to_string(),
                image: image.to_string(),
                volume,
                ports,
                allocated,
                created_at,
                rate_micro_usdc_per_hour,
                budget_micro_usdc,
            };
            self.vms.lock().unwrap().insert(owner, rec);
            adopted += 1;
        }
        adopted
    }

    /// Spawn an interactive shell inside the VM on a real pseudo-terminal, so
    /// full-screen programs (vim, htop, less) work and the shell handles echo
    /// and line editing. Caller pumps bytes between the QUIC session stream
    /// and the PTY master.
    pub async fn open_shell(
        &self,
        owner: &str,
        command: &[String],
        cols: u16,
        rows: u16,
    ) -> Result<PtySession> {
        let name = {
            let vms = self.vms.lock().unwrap();
            vms.get(owner)
                .map(|r| r.vm_id.clone())
                .ok_or_else(|| anyhow!("no VM for this identity — run `cloudiy vm up` first"))?
        };
        validate_command(command)?;
        let binary = self.binary.clone();
        let command: Vec<String> = command.to_vec();
        let cols = if cols == 0 { 80 } else { cols };
        let rows = if rows == 0 { 24 } else { rows };

        // portable-pty is a blocking API — build the session off the runtime.
        tokio::task::spawn_blocking(move || open_pty(&binary, &name, &command, cols, rows))
            .await
            .context("pty task panicked")?
    }
}

/// A live pseudo-terminal session: the master side (for resize) plus its
/// reader/writer halves and the child `docker exec` process.
pub struct PtySession {
    pub master: Box<dyn portable_pty::MasterPty + Send>,
    pub reader: Box<dyn std::io::Read + Send>,
    pub writer: Box<dyn std::io::Write + Send>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
}

fn open_pty(
    binary: &str,
    container: &str,
    command: &[String],
    cols: u16,
    rows: u16,
) -> Result<PtySession> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(binary);
    cmd.arg("exec");
    cmd.arg("-it");
    cmd.arg(container);
    if command.is_empty() {
        // Prefer bash, fall back to sh — a real login-ish interactive shell.
        cmd.arg("/bin/sh");
        cmd.arg("-c");
        cmd.arg("if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi");
    } else {
        for a in command {
            cmd.arg(a);
        }
    }
    cmd.env("TERM", "xterm-256color");

    let child = pair.slave.spawn_command(cmd)?;
    // Close the slave in the parent so EOF propagates when the child exits.
    drop(pair.slave);
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    Ok(PtySession {
        master: pair.master,
        reader,
        writer,
        child,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(rate: u64, budget: u64, age_secs: i64) -> VmRecord {
        VmRecord {
            vm_id: "vm".into(),
            image: "debian".into(),
            volume: "vol".into(),
            ports: vec![],
            allocated: ResourceVector::new(),
            created_at: chrono::Utc::now() - chrono::Duration::seconds(age_secs),
            rate_micro_usdc_per_hour: rate,
            budget_micro_usdc: budget,
        }
    }

    #[test]
    fn lease_accrues_and_expires() {
        let now = chrono::Utc::now();
        // 3600 micro-USDC/h → 1 micro-USDC per second.
        let r = record(3600, 1800, 1810); // paid 1800s, ran 1810s → spent
        assert!(r.accrued(now) >= 1800);
        assert!(r.exhausted(now), "lease should be spent");
        assert_eq!(r.remaining_secs(now), Some(0));

        // Ran only half the budget → not exhausted, ~half remaining.
        let r = record(3600, 1800, 900);
        assert!(!r.exhausted(now));
        let left = r.remaining_secs(now).unwrap();
        assert!((890..=910).contains(&left), "≈900s left, got {left}");
    }

    #[test]
    fn zero_budget_is_unmetered() {
        let now = chrono::Utc::now();
        let r = record(3600, 0, 100_000);
        assert!(!r.exhausted(now), "dev-mode VM never expires");
        assert_eq!(r.remaining_secs(now), None);
    }

    #[test]
    fn zero_rate_never_charges() {
        let now = chrono::Utc::now();
        let r = record(0, 1000, 100_000);
        assert_eq!(r.accrued(now), 0);
        assert!(!r.exhausted(now));
        assert_eq!(r.remaining_secs(now), None);
    }
}
