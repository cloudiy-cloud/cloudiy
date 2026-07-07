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

/// Detected hardware as protocol resources. MVP shares everything detected;
/// the remaining-private split is a CLI flag away (`Resources::declare`
/// already enforces shared ≤ total).
pub fn detect_resources(gpu_count: u64, vram_mib: u64) -> Resources {
    let mut total = ResourceVector::new()
        .with(ResourceKind::Cpu, cpu_millicores())
        .with(ResourceKind::Memory, total_memory_mib());
    if gpu_count > 0 {
        total = total
            .with(ResourceKind::Gpu, gpu_count)
            .with(ResourceKind::Vram, vram_mib);
    }
    Resources::declare(total.clone(), total).expect("shared == total is always valid")
}

/// Detected functionality as protocol capabilities.
pub async fn detect_capabilities() -> Vec<Capability> {
    let mut caps: Vec<Capability> = vec![
        // The built-in wgpu/WGSL kernel executor is itself a runtime.
        "wgsl".into(),
        std::env::consts::OS.into(),   // linux / macos / windows
        std::env::consts::ARCH.into(), // x86_64 / aarch64
    ];
    // Each shipped kernel is an addressable capability (`kernel:vector_add`),
    // so schedulers can match template workloads without probing.
    for kernel in cloudiy_runtime::KERNELS {
        caps.push(Capability::new(format!("kernel:{kernel}")));
    }
    if DockerRuntime::default().supports().await {
        caps.push("docker".into());
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cpu_and_memory() {
        let r = detect_resources(1, 8_192);
        assert!(r.total.get(&ResourceKind::Cpu) >= 1000, "at least one core");
        assert_eq!(r.total.get(&ResourceKind::Gpu), 1);
        assert_eq!(r.available(), r.total, "MVP shares everything");
    }

    #[tokio::test]
    async fn capabilities_include_os_and_arch() {
        let caps = detect_capabilities().await;
        let names: Vec<&str> = caps.iter().map(|c| c.name()).collect();
        assert!(names.contains(&"wgsl"));
        assert!(names.contains(&std::env::consts::OS));
        assert!(names.contains(&std::env::consts::ARCH));
    }
}
