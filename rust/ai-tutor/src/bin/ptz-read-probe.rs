//! Standalone bounded read-only query; it does not control the running workbench.
#[path = "../native_ptz_read.rs"]
mod native_ptz_read;
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Job(isize);
impl Job {
    fn attach(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(!handle.is_null(), "Cannot create read-probe job");
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        };
        if ok == 0 {
            unsafe { CloseHandle(handle) };
            return Err(anyhow!("Cannot configure read-probe job"));
        }
        if unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } == 0 {
            unsafe { CloseHandle(handle) };
            return Err(anyhow!("Cannot attach read-probe job"));
        }
        Ok(Self(handle as isize))
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0 as _) };
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    root: Option<PathBuf>,
    output: Option<PathBuf>,
    worker: bool,
    once: bool,
    stdout_only: bool,
    help: bool,
}
fn parse_args(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--root" | "--output" => {
                let flag = &args[i];
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("{flag} requires a path"))?;
                ensure!(
                    !value.is_empty() && !value.starts_with("--"),
                    "{flag} requires a path"
                );
                let target = if flag == "--root" {
                    &mut options.root
                } else {
                    &mut options.output
                };
                ensure!(target.is_none(), "Duplicate option: {flag}");
                *target = Some(PathBuf::from(value));
                i += 2;
            }
            "--worker" => {
                ensure!(!options.worker, "Duplicate --worker");
                options.worker = true;
                i += 1;
            }
            "--once" => {
                ensure!(!options.once, "Duplicate --once");
                options.once = true;
                i += 1;
            }
            "--stdout-only" => {
                ensure!(!options.stdout_only, "Duplicate --stdout-only");
                options.stdout_only = true;
                i += 1;
            }
            "--help" | "-h" => {
                ensure!(!options.help, "Duplicate help option");
                options.help = true;
                i += 1;
            }
            other => return Err(anyhow!("Unknown argument: {other}")),
        }
    }
    ensure!(
        !(options.worker && options.output.is_some()),
        "Internal worker cannot accept an output path"
    );
    ensure!(
        !options.stdout_only || (options.once && options.output.is_none() && !options.worker),
        "--stdout-only requires --once and cannot use --output or --worker"
    );
    ensure!(
        !options.help
            || (options.root.is_none()
                && options.output.is_none()
                && !options.worker
                && !options.once
                && !options.stdout_only),
        "Help cannot be combined with other options"
    );
    Ok(options)
}

fn resolve_output(root: &Path, requested: Option<&Path>) -> Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    let requested = requested.unwrap_or_else(|| Path::new("runtime/ptz-read-rust.json"));
    ensure!(
        !requested
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "Output path cannot contain parent traversal"
    );
    let candidate = if requested.is_absolute() {
        requested.to_owned()
    } else {
        root.join(requested)
    };
    let filename = candidate.file_name().context("Output must name a file")?;
    let mut ancestor = candidate
        .parent()
        .context("Output must have a parent directory")?;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        missing.push(
            ancestor
                .file_name()
                .context("Invalid output directory")?
                .to_owned(),
        );
        ancestor = ancestor.parent().context("No existing output ancestor")?;
    }
    let mut parent = fs::canonicalize(ancestor)?;
    ensure!(
        parent.starts_with(&root),
        "Output path must stay inside the project root"
    );
    for part in missing.iter().rev() {
        parent.push(part);
    }
    fs::create_dir_all(&parent)?;
    let parent = fs::canonicalize(parent)?;
    ensure!(
        parent.starts_with(&root),
        "Output directory resolves outside the project root"
    );
    let output = parent.join(filename);
    match fs::symlink_metadata(&output) {
        Ok(_) => {
            return Err(anyhow!(
                "Output already exists; choose a fresh --output path: {}",
                output.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(output)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let options = parse_args(&args)?;
    if options.help {
        println!(
            "ptz-read-probe [--root PROJECT] [--output RELATIVE_OR_ABSOLUTE_PATH_WITHIN_PROJECT] [--once]\nptz-read-probe --root PROJECT --once --stdout-only\nRead-only DirectShow GetRange/Get; no capture or motion. --once reads one sample; --stdout-only creates no evidence file and requires --once. Existing output files are never overwritten. Unknown, duplicate and incomplete arguments are rejected before hardware access."
        );
        return Ok(());
    }
    let root = fs::canonicalize(options.root.unwrap_or(std::env::current_dir()?))?;
    if options.worker {
        let mut marker = [0u8; 4];
        std::io::stdin().read_exact(&mut marker)?;
        ensure!(
            marker == *b"RUN\n",
            "Parent did not release read-only job gate"
        );
        let result = native_ptz_read::existing_device_path(&root).and_then(|path| {
            native_ptz_read::read_only_probe(
                &path,
                if options.once { 1 } else { 12 },
                if options.once {
                    Duration::ZERO
                } else {
                    Duration::from_millis(250)
                },
            )
        });
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                json!({"result":"FAIL","read_only":true,"set_calls":0,"capture_graph_created":false,"capture_stream_started":false,"error":error,"implementation":"Rust native DirectShow"})
            }
        };
        println!("{report}");
        return Ok(());
    }
    // Validate the exact destination before spawning or touching the controller.
    let output = if options.stdout_only {
        None
    } else {
        Some(resolve_output(&root, options.output.as_deref())?)
    };
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--worker")
        .arg("--root")
        .arg(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000);
    if options.once {
        command.arg("--once");
    }
    let mut child = command.spawn()?;
    let _job = match Job::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    {
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Missing read-probe launch pipe"))?;
        input.write_all(b"RUN\n")?;
        input.flush()?;
    }
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut data = Vec::new();
        stdout.take(128 * 1024).read_to_end(&mut data).map(|_| data)
    });
    let err = thread::spawn(move || {
        let mut data = Vec::new();
        stderr.take(8192).read_to_end(&mut data).map(|_| data)
    });
    let start = Instant::now();
    let exit = loop {
        if let Some(exit) = child.try_wait()? {
            break exit;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "Read-only native query exceeded20s; owned process reclaimed"
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = out
        .join()
        .map_err(|_| anyhow!("Read-probe output thread failed"))??;
    let errors = err
        .join()
        .map_err(|_| anyhow!("Read-probe diagnostics thread failed"))??;
    ensure!(
        exit.success(),
        "Read-probe failed: {}",
        String::from_utf8_lossy(&errors)
    );
    let report: Value = serde_json::from_slice(&bytes)?;
    if let Some(output) = &output {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        file.write_all(&serde_json::to_vec_pretty(&report)?)?;
        file.flush()?;
    }
    if options.stdout_only {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    ensure!(
        report["result"] == "PASS_READ_ONLY_QUERY",
        "Native query did not pass; see {}",
        output
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "stdout report".into())
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(words: &[&str]) -> Vec<String> {
        std::iter::once("ptz-read-probe")
            .chain(words.iter().copied())
            .map(str::to_owned)
            .collect()
    }
    #[test]
    fn unknown_duplicate_and_incomplete_arguments_are_rejected() {
        for input in [
            vec!["--out", "fresh.json"],
            vec!["--root"],
            vec!["--output", "--worker"],
            vec!["--root", "a", "--root", "b"],
            vec!["--output", "a", "--output", "b"],
            vec!["--worker", "--worker"],
            vec!["extra"],
            vec!["--worker", "--output", "a"],
            vec!["--help", "--root", "a"],
            vec!["--once", "--once"],
            vec!["--stdout-only"],
            vec!["--once", "--stdout-only", "--stdout-only"],
            vec!["--once", "--stdout-only", "--output", "a"],
            vec!["--worker", "--once", "--stdout-only"],
            vec!["--help", "--once"],
        ] {
            assert!(parse_args(&args(&input)).is_err(), "{input:?}")
        }
    }
    #[test]
    fn explicit_root_and_output_are_preserved() {
        let parsed = parse_args(&args(&[
            "--root",
            "project root",
            "--output",
            "runtime/fresh.json",
        ]))
        .unwrap();
        assert_eq!(parsed.root, Some(PathBuf::from("project root")));
        assert_eq!(parsed.output, Some(PathBuf::from("runtime/fresh.json")));
        assert!(!parsed.worker);
    }
    #[test]
    fn one_shot_stdout_mode_is_explicit_and_does_not_resolve_a_file() {
        let parsed = parse_args(&args(&[
            "--root",
            "project root",
            "--once",
            "--stdout-only",
        ]))
        .unwrap();
        assert!(parsed.once && parsed.stdout_only && !parsed.worker);
        assert_eq!(parsed.output, None);
        let worker = parse_args(&args(&["--root", "project root", "--worker", "--once"])).unwrap();
        assert!(worker.once && worker.worker && !worker.stdout_only);
    }
    #[test]
    fn output_resolves_under_root_and_existing_evidence_is_preserved() {
        let root = tempfile::tempdir().unwrap();
        let fresh = resolve_output(root.path(), Some(Path::new("runtime/new/fresh.json"))).unwrap();
        assert!(fresh.starts_with(fs::canonicalize(root.path()).unwrap()));
        fs::write(&fresh, "GENUINE_EXISTING_EVIDENCE").unwrap();
        assert!(resolve_output(root.path(), Some(Path::new("runtime/new/fresh.json"))).is_err());
        assert_eq!(
            fs::read_to_string(&fresh).unwrap(),
            "GENUINE_EXISTING_EVIDENCE"
        );
        assert!(resolve_output(root.path(), Some(Path::new("../escape.json"))).is_err());
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(resolve_output(root.path(), Some(&elsewhere.path().join("outside.json"))).is_err());
    }
}
