//! Native read-only sidecar. Samples process resources; never opens a device or database.
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

fn descendants(root: u32, parents: &HashMap<u32, Option<u32>>) -> HashSet<u32> {
    let mut found = HashSet::from([root]);
    loop {
        let before = found.len();
        for (&pid, &parent) in parents {
            if parent.is_some_and(|p| found.contains(&p)) {
                found.insert(pid);
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

fn soak_progress(root: &Path) -> Result<(f64, f64)> {
    let path = root.join("runtime/soak-rust.jsonl");
    let mut file = File::open(&path)?;
    let metadata = file.metadata()?;
    let length = metadata.len();
    if length > 65536 {
        file.seek(SeekFrom::End(-65536))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let latest = String::from_utf8_lossy(&bytes)
        .lines()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v["elapsed"].as_f64())
        })
        .ok_or_else(|| anyhow!("No valid native-soak elapsed sample"))?;
    let file_age = metadata
        .modified()?
        .elapsed()
        .unwrap_or_default()
        .as_secs_f64();
    Ok((latest, latest + file_age))
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn mib(bytes: u64) -> f64 {
    (bytes as f64 / 1048576.0 * 1000.0).round() / 1000.0
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let root = fs::canonicalize(
        flag(&args, "--root")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?),
    )?;
    let boundary = flag(&args, "--boundary-seconds")
        .map(|v| v.parse::<f64>())
        .transpose()?
        .unwrap_or(1800.0);
    ensure!(
        boundary.is_finite() && boundary > 0.0,
        "Invalid soak boundary"
    );
    let descriptor: Value = serde_json::from_slice(&fs::read(root.join("runtime/server.json"))?)?;
    ensure!(
        descriptor["implementation"]
            .as_str()
            .is_some_and(|v| v.starts_with("Rust")),
        "Resource monitor requires the native Rust service"
    );
    let root_pid = descriptor["pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| anyhow!("Invalid native service PID"))?;
    let (soak_at_start, estimated_at_start) = soak_progress(&root)?;
    let once = args.iter().any(|a| a == "--once");
    let output = flag(&args, "--output")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("runtime/rust-resources.jsonl"));
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut log = OpenOptions::new().create(true).append(true).open(&output)?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64((boundary - estimated_at_start).max(0.0));
    let kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet);
    let mut system = System::new();
    system.refresh_cpu_all();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    let original_start = system
        .process(Pid::from_u32(root_pid))
        .context("Native service is not running")?
        .start_time();
    let mut known_pids: HashSet<_> = system.processes().keys().copied().collect();
    let mut last_refresh = Instant::now();
    thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL + Duration::from_millis(50));
    let mut samples = 0u64;
    loop {
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        let interval = last_refresh.elapsed().as_secs_f64();
        last_refresh = Instant::now();
        let root_alive = system
            .process(Pid::from_u32(root_pid))
            .is_some_and(|p| p.start_time() == original_start);
        let parents: HashMap<_, _> = system
            .processes()
            .iter()
            .map(|(pid, p)| (pid.as_u32(), p.parent().map(Pid::as_u32)))
            .collect();
        let tree = descendants(root_pid, &parents);
        let mut processes = Vec::new();
        let mut total = 0u64;
        let mut root_rss = 0u64;
        let mut vision_rss = 0u64;
        let mut cpu_sum = 0.0f64;
        let mut cpu_unknown = 0;
        for pid in tree {
            let native_pid = Pid::from_u32(pid);
            let Some(process) = system.process(native_pid) else {
                continue;
            };
            let is_vision = process.cmd().iter().any(|arg| arg == "--camera-worker");
            let name = process.name().to_string_lossy().into_owned();
            let role = if pid == root_pid {
                "server"
            } else if is_vision {
                "vision_worker"
            } else if name.to_lowercase().contains("kimi") {
                "kimi_provider"
            } else {
                "descendant"
            };
            let bytes = process.memory();
            total = total.saturating_add(bytes);
            if pid == root_pid {
                root_rss = bytes;
            }
            if is_vision {
                vision_rss = vision_rss.saturating_add(bytes);
            }
            let valid_cpu = known_pids.contains(&native_pid);
            let cpu = if valid_cpu {
                let v = process.cpu_usage() as f64;
                cpu_sum += v;
                json!(v)
            } else {
                cpu_unknown += 1;
                Value::Null
            };
            // Never write full command lines: Kimi may carry teaching text as an argument.
            processes.push(json!({"pid":pid,"parent_pid":process.parent().map(Pid::as_u32),"name":name,"role":role,"working_set_bytes":bytes,"working_set_mb":mib(bytes),"cpu_percent_one_logical_core":cpu,"start_time_unix_seconds":process.start_time()}));
        }
        processes.sort_by_key(|p| p["pid"].as_u64());
        let (soak_latest, soak_estimate) = soak_progress(&root).unwrap_or((
            soak_at_start,
            estimated_at_start + started.elapsed().as_secs_f64(),
        ));
        let cores = system.cpus().len().max(1);
        let final_sample =
            !root_alive || Instant::now() >= deadline || soak_latest >= boundary || once;
        let row = json!({"schema_version":"1.0","monitor_run_id":run_id,"monitor_pid":std::process::id(),"timestamp_utc":chrono::Utc::now().to_rfc3339(),
            "root_pid":root_pid,"root_process_alive_same_start_time":root_alive,"monitor_elapsed_seconds":started.elapsed().as_secs_f64(),"soak_elapsed_latest_sample":soak_latest,"soak_elapsed_estimated_from_file_age":soak_estimate,
            "resource_sampling_started_after_soak_seconds":soak_at_start,"startup_resource_samples_available":false,"sample_interval_seconds":interval,"nominal_interval_seconds":5,
            "root_process_working_set_mb":mib(root_rss),"vision_workers_working_set_mb":mib(vision_rss),"sum_process_working_sets_mb":mib(total),"sum_process_working_sets_bytes":total,
            "working_set_caveat":"Sum of root and descendant resident working sets; shared pages may be counted more than once. This is not unique physical RAM and excludes browser/monitor processes.",
            "cpu_logical_sum_percent":cpu_sum,"cpu_unknown_process_count":cpu_unknown,"logical_cpu_count":cores,"cpu_estimated_percent_of_machine_capacity":cpu_sum/cores as f64,
            "cpu_caveat":"100 percent is one logical CPU; summed known process usage may exceed 100. Newly observed processes have null CPU until a later refresh; 5-second sampling may miss short-lived peaks.",
            "processes":processes,"final_sample":final_sample,"stop_reason":if !root_alive{Some("native_service_exited_or_pid_reused")}else if once{Some("single_sample")}else if final_sample{Some("native_soak_30_minute_boundary")}else{None::<&str>}});
        writeln!(log, "{row}")?;
        log.flush()?;
        samples += 1;
        if final_sample {
            break;
        }
        known_pids = system.processes().keys().copied().collect();
        thread::sleep(
            Duration::from_secs(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    println!(
        "{}",
        json!({"resource_monitor_complete":true,"samples":samples,"root_pid":root_pid,"log":output,"sampled_after_soak_seconds":soak_at_start,"elapsed_seconds":started.elapsed().as_secs_f64()})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_includes_all_descendants_and_excludes_unrelated_processes() {
        let parents = HashMap::from([
            (1, None),
            (2, Some(1)),
            (3, Some(2)),
            (4, Some(9)),
            (9, None),
        ]);
        assert_eq!(descendants(1, &parents), HashSet::from([1, 2, 3]));
    }
    #[test]
    fn cycles_cannot_make_discovery_unbounded() {
        let parents = HashMap::from([(1, Some(2)), (2, Some(1)), (3, Some(2))]);
        assert_eq!(descendants(1, &parents), HashSet::from([1, 2, 3]));
        assert_eq!(descendants(8, &parents), HashSet::from([8]));
    }
}
