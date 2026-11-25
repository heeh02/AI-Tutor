//! Only library-test diagnostics need a separate release camera-worker entry.
//! The override is local to one fixture and pins the exact executable bytes.
use super::*;
use serde::Deserialize;

#[cfg(windows)]
#[derive(Default)]
pub(super) struct OwnedReceipt {
    pub(super) child_started: AtomicU64,
    child_wait_confirmed: AtomicU64,
    jobs_assigned: AtomicU64,
    jobs_empty: AtomicU64,
    supervisor_exited: AtomicBool,
    pub(super) accepted_processed_frames: AtomicU64,
}
#[cfg(windows)]
impl OwnedReceipt {
    pub(super) fn waited(&self, confirmed: bool) {
        if confirmed {
            self.child_wait_confirmed.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn snapshot(&self) -> Value {
        let started = self.child_started.load(Ordering::SeqCst);
        let waited = self.child_wait_confirmed.load(Ordering::SeqCst);
        let assigned = self.jobs_assigned.load(Ordering::SeqCst);
        let empty = self.jobs_empty.load(Ordering::SeqCst);
        let exited = self.supervisor_exited.load(Ordering::SeqCst);
        json!({"child_started_count":started,"child_wait_confirmed_count":waited,
            "jobs_assigned_count":assigned,"jobs_empty_count":empty,"supervisor_exited":exited,
            "cleanup_confirmed":exited && started==waited && assigned==empty,
            "accepted_processed_frames":self.accepted_processed_frames.load(Ordering::SeqCst)})
    }
}
#[cfg(windows)]
pub(super) struct SupervisorReceipt(pub(super) Arc<Control>);
#[cfg(windows)]
impl Drop for SupervisorReceipt {
    fn drop(&mut self) {
        self.0
            .diagnostic
            .supervisor_exited
            .store(true, Ordering::SeqCst);
    }
}
#[cfg(windows)]
pub(super) struct JobReceipt<'a> {
    job: &'a ProcessJob,
    control: &'a Control,
}
#[cfg(windows)]
impl<'a> JobReceipt<'a> {
    pub(super) fn new(job: &'a ProcessJob, control: &'a Control) -> Self {
        control
            .diagnostic
            .jobs_assigned
            .fetch_add(1, Ordering::SeqCst);
        Self { job, control }
    }
}
#[cfg(windows)]
impl Drop for JobReceipt<'_> {
    fn drop(&mut self) {
        use windows_sys::Win32::System::JobObjects::*;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                QueryInformationJobObject(
                    self.job.handle as _,
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 && info.ActiveProcesses == 0 {
                self.control
                    .diagnostic
                    .jobs_empty
                    .fetch_add(1, Ordering::SeqCst);
                break;
            }
            if ok == 0 || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
#[cfg(windows)]
impl CameraService {
    pub(crate) fn diagnostic_owner_status(&self) -> Value {
        self.owner
            .lock()
            .unwrap()
            .as_ref()
            .map_or(Value::Null, |c| c.diagnostic.snapshot())
    }
    /// Exact Child::wait and still-owned Job accounting; no PID-based cleanup.
    pub(crate) async fn close_diagnostic(&self) -> Value {
        let control = self.owner.lock().unwrap().clone();
        self.close();
        let Some(control) = control else {
            return json!({"not_started":true,"cleanup_confirmed":true});
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !control.diagnostic.supervisor_exited.load(Ordering::SeqCst)
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        control.diagnostic.snapshot()
    }
}

#[cfg(windows)]
#[test]
fn cleanup_receipt_requires_wait_job_empty_and_supervisor_not_just_closed_state() {
    let receipt = OwnedReceipt::default();
    receipt.child_started.store(1, Ordering::SeqCst);
    receipt.jobs_assigned.store(1, Ordering::SeqCst);
    assert_eq!(receipt.snapshot()["cleanup_confirmed"], false);
    receipt.waited(true);
    receipt.jobs_empty.store(1, Ordering::SeqCst);
    assert_eq!(receipt.snapshot()["cleanup_confirmed"], false);
    receipt.supervisor_exited.store(true, Ordering::SeqCst);
    assert_eq!(receipt.snapshot()["cleanup_confirmed"], true);
    receipt.child_started.store(2, Ordering::SeqCst);
    assert_eq!(receipt.snapshot()["cleanup_confirmed"], false);
}

#[cfg(windows)]
#[tokio::test]
async fn diagnostic_camera_close_waits_for_exact_idle_child_and_empty_owned_job() {
    use std::os::windows::process::CommandExt;
    let executable = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32/ping.exe");
    let mut child = Command::new(executable)
        .args(["127.0.0.1", "-n", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    let job = match ProcessJob::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{error}")
        }
    };
    let service = CameraService::new(PathBuf::from("unused-no-camera"));
    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        child: Mutex::new(Some(child)),
        last_packet: Mutex::new(Instant::now()),
        diagnostic: OwnedReceipt::default(),
    });
    control.diagnostic.child_started.store(1, Ordering::SeqCst);
    *service.owner.lock().unwrap() = Some(control.clone());
    let observer = control.clone();
    let supervisor = thread::spawn(move || {
        let _exited = SupervisorReceipt(observer.clone());
        let _job = JobReceipt::new(&job, &observer);
        let deadline = Instant::now() + Duration::from_secs(4);
        while !observer.stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        // close_diagnostic owns the only Child wait; no PID lookup or device.
        while Instant::now() < deadline
            && observer
                .diagnostic
                .child_wait_confirmed
                .load(Ordering::SeqCst)
                == 0
        {
            thread::sleep(Duration::from_millis(1));
        }
    });
    let receipt = service.close_diagnostic().await;
    supervisor.join().unwrap();
    assert_eq!(receipt["cleanup_confirmed"], true);
    assert_eq!(receipt["child_started_count"], 1);
    assert_eq!(receipt["child_wait_confirmed_count"], 1);
    assert_eq!(receipt["jobs_empty_count"], 1);
    assert_eq!(receipt["accepted_processed_frames"], 0);
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Worker {
    protocol_version: u8,
    executable: PathBuf,
    sha256: String,
}

pub(super) fn resolve(root: &Path, default: PathBuf) -> Result<PathBuf, String> {
    let path = root.join("runtime/diagnostic-camera-worker.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(default),
        Err(error) => return Err(error.to_string()),
    };
    let worker: Worker = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if worker.protocol_version != 1 || !worker.executable.is_absolute() {
        return Err("Diagnostic camera worker requires version1 and an absolute path".into());
    }
    let binary = fs::read(&worker.executable).map_err(|e| e.to_string())?;
    let actual = format!("{:x}", Sha256::digest(&binary));
    if actual != worker.sha256 {
        return Err("Diagnostic camera worker executable hash changed".into());
    }
    Ok(worker.executable)
}

#[test]
fn fixture_override_is_hash_bound_and_does_not_affect_another_camera_instance() {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    let fallback = PathBuf::from("untouched-default");
    assert_eq!(resolve(one.path(), fallback.clone()).unwrap(), fallback);
    fs::create_dir(one.path().join("runtime")).unwrap();
    let binary = one.path().join("synthetic-not-executable.bin");
    fs::write(&binary, b"fixed diagnostic bytes; never executed").unwrap();
    let expected = format!("{:x}", Sha256::digest(fs::read(&binary).unwrap()));
    fs::write(
        one.path().join("runtime/diagnostic-camera-worker.json"),
        json!({"protocol_version":1,"executable":binary,"sha256":expected}).to_string(),
    )
    .unwrap();
    assert_eq!(resolve(one.path(), fallback.clone()).unwrap(), binary);
    assert_eq!(resolve(two.path(), fallback.clone()).unwrap(), fallback);
    fs::write(&binary, b"changed bytes").unwrap();
    assert!(resolve(one.path(), fallback).is_err());
}
