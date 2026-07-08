//! Node discovery: what this machine has (resources) and what it can do
//! (capabilities). The provider daemon announces the result; the slice
//! actually shared is chosen by the provider (MVP: everything detected —
//! `--share-*` flags come next).

use cloudiy_protocol::{Capability, ResourceKind, ResourceVector, Resources};
use cloudiy_runtime::{DockerRuntime, Runtime};

/// Total system memory in MiB (best effort, 0 when unknown).
fn total_memory_mib() -> u64 {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
        {
            if let Ok(bytes) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                return bytes / (1024 * 1024);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            if let Some(kb) = meminfo
                .lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
            {
                return kb / 1024;
            }
        }
    }
    0
}

fn cpu_millicores() -> u64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u64 * 1000)
        .unwrap_or(0)
}

/// Detected hardware as protocol resources. The provider chooses the shared
/// slice: `share_cpu_millis` / `share_memory_mib` cap what the network may
/// use (clamped to what exists); `None` shares everything detected. What is
/// not shared stays private — the protocol never assumes the whole machine.
pub fn detect_resources(
    gpu_count: u64,
    vram_mib: u64,
    share_cpu_millis: Option<u64>,
    share_memory_mib: Option<u64>,
) -> Resources {
    let mut total = ResourceVector::new()
        .with(ResourceKind::Cpu, cpu_millicores())
        .with(ResourceKind::Memory, total_memory_mib());
    if gpu_count > 0 {
        total = total
            .with(ResourceKind::Gpu, gpu_count)
            .with(ResourceKind::Vram, vram_mib);
    }

    let mut shared = total.clone();
    if let Some(cpu) = share_cpu_millis {
        shared.0.insert(ResourceKind::Cpu, cpu.min(total.get(&ResourceKind::Cpu)));
    }
    if let Some(mem) = share_memory_mib {
        shared
            .0
            .insert(ResourceKind::Memory, mem.min(total.get(&ResourceKind::Memory)));
    }
    Resources::declare(total, shared).expect("shared is clamped to total")
}

/// Detected functionality as protocol capabilities. GPU-less nodes simply
/// don't announce `wgsl`/`kernel:*` — CPU/RAM providers are first-class,
/// the scheduler routes accordingly.
pub async fn detect_capabilities(has_gpu: bool, container_runtime: Option<&str>) -> Vec<Capability> {
    let mut caps: Vec<Capability> = vec![
        std::env::consts::OS.into(),   // linux / macos / windows
        std::env::consts::ARCH.into(), // x86_64 / aarch64
    ];
    if has_gpu {
        // The built-in wgpu/WGSL kernel executor is itself a runtime, and
        // each shipped kernel is an addressable capability
        // (`kernel:vector_add`), so schedulers match without probing.
        caps.push("wgsl".into());
        for kernel in cloudiy_runtime::KERNELS {
            caps.push(Capability::new(format!("kernel:{kernel}")));
        }
    }
    if DockerRuntime::default().supports().await {
        caps.push("docker".into());
        // Advertise the isolation level so a workload can *require* a sandbox
        // (`isolation:gvisor`) and the scheduler routes accordingly.
        caps.push(Capability::new(isolation_level(container_runtime)));
    }
    caps
}

/// Map an OCI runtime to the isolation level it provides.
pub fn isolation_level(runtime: Option<&str>) -> String {
    match runtime {
        Some(r) if r.contains("runsc") || r.contains("gvisor") => "isolation:gvisor".into(),
        Some(r) if r.contains("kata") => "isolation:kata".into(),
        // Plain runc (explicit or default) is standard container isolation.
        None | Some("runc") => "isolation:container".into(),
        Some(other) => format!("isolation:{other}"),
    }
}

/// OCI runtimes Docker knows about (`docker info`), for a startup check.
pub async fn detect_docker_runtimes() -> Vec<String> {
    let out = tokio::process::Command::new("docker")
        .args(["info", "--format", "{{json .Runtimes}}"])
        .output()
        .await;
    let Ok(out) = out else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(text.trim())
        .map(|m| m.into_keys().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cpu_and_memory() {
        let r = detect_resources(1, 8_192, None, None);
        assert!(r.total.get(&ResourceKind::Cpu) >= 1000, "at least one core");
        assert_eq!(r.total.get(&ResourceKind::Gpu), 1);
        assert_eq!(r.available(), r.total, "default shares everything");
    }

    #[test]
    fn cpu_only_node_announces_no_gpu() {
        let r = detect_resources(0, 0, None, None);
        assert_eq!(r.total.get(&ResourceKind::Gpu), 0);
        assert_eq!(r.total.get(&ResourceKind::Vram), 0);
        assert!(r.total.get(&ResourceKind::Cpu) >= 1000);
    }

    #[test]
    fn shared_slice_is_capped_and_clamped() {
        // Share 2 cores and 1 GiB; keep the rest private.
        let r = detect_resources(1, 8_192, Some(2_000), Some(1_024));
        assert_eq!(r.available().get(&ResourceKind::Cpu), 2_000);
        assert_eq!(r.available().get(&ResourceKind::Memory), 1_024);
        // Asking to share more than exists clamps to the total.
        let r = detect_resources(1, 8_192, Some(u64::MAX), Some(u64::MAX));
        assert_eq!(r.available(), r.total);
    }

    #[tokio::test]
    async fn capabilities_follow_the_hardware() {
        let with_gpu = detect_capabilities(true, None).await;
        let names: Vec<&str> = with_gpu.iter().map(|c| c.name()).collect();
        assert!(names.contains(&"wgsl"));
        assert!(names.contains(&std::env::consts::OS));

        let without_gpu = detect_capabilities(false, None).await;
        let names: Vec<&str> = without_gpu.iter().map(|c| c.name()).collect();
        assert!(!names.contains(&"wgsl"));
        assert!(!names.iter().any(|n| n.starts_with("kernel")));
        assert!(names.contains(&std::env::consts::ARCH));
    }

    #[test]
    fn isolation_level_maps_runtimes() {
        assert_eq!(isolation_level(None), "isolation:container");
        assert_eq!(isolation_level(Some("runsc")), "isolation:gvisor");
        assert_eq!(isolation_level(Some("kata-runtime")), "isolation:kata");
        assert_eq!(isolation_level(Some("crun")), "isolation:crun");
    }
}
