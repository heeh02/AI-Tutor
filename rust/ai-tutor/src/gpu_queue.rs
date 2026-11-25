//! Serialize managed GPU queue acceptance, not model work or shutdown.
use anyhow::{Result, ensure};
use serde_json::Value;

#[derive(Default)]
pub(super) struct GpuQueuePin {
    distribution: Option<String>,
}

fn cleanup_unknown(status: &Value) -> bool {
    status["phase"] == "cleanup_failed"
        || status["cleanup_confirmed"] == false
        || status["linux_cleanup_confirmed"] == false
}

fn safely_idle(status: &Value) -> bool {
    status["busy"] == false
        && status["active"] != true
        && !cleanup_unknown(status)
        && [
            "disabled",
            "configured",
            "configuration_error",
            "stopped",
            "degraded",
        ]
        .contains(&status["phase"].as_str().unwrap_or(""))
}

impl GpuQueuePin {
    pub(super) fn refresh_if_idle(&mut self, speech: &Value, asr: &Value) {
        if safely_idle(speech) && safely_idle(asr) {
            self.distribution = None;
        }
    }
    /// Called under the Workstation queue mutex before either role publishes
    /// running/task. Inputs are runtime snapshots; disk peer settings are not proof.
    pub(super) fn admit(
        &mut self,
        captured_distribution: &str,
        speech: &Value,
        asr: &Value,
    ) -> Result<()> {
        ensure!(
            !cleanup_unknown(speech) && !cleanup_unknown(asr),
            "GPU 模型进程退出尚未确认"
        );
        self.refresh_if_idle(speech, asr);
        ensure!(
            !captured_distribution.is_empty(),
            "缺少 GPU 模型的 WSL 发行版"
        );
        if let Some(pinned) = &self.distribution {
            ensure!(
                pinned.eq_ignore_ascii_case(captured_distribution),
                "请先停止两路 GPU 模型，再更换 WSL 发行版"
            );
        } else {
            self.distribution = Some(captured_distribution.to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Barrier, Mutex};

    fn idle() -> Value {
        json!({"phase":"stopped","busy":false,"active":false,"cleanup_confirmed":true})
    }
    fn busy(phase: &str) -> Value {
        json!({"phase":phase,"busy":true,"active":phase=="active"})
    }

    fn policies(
        root: &std::path::Path,
        distribution: &str,
        asr_enabled: bool,
        mode: &str,
    ) -> Result<String> {
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime)?;
        std::fs::write(
            runtime.join("gpu-resource-policy.json"),
            serde_json::to_vec(&json!({"protocol_version":1,"mode":mode}))?,
        )?;
        std::fs::write(
            runtime.join("local-speech-policy.json"),
            serde_json::to_vec(&json!({"mode":"managed","managed":{"distribution":distribution}}))?,
        )?;
        std::fs::write(
            runtime.join("local-final-asr-policy.json"),
            serde_json::to_vec(
                &json!({"enabled":asr_enabled,"managed":{"distribution":distribution}}),
            )?,
        )?;
        let queued: Value =
            serde_json::from_slice(&std::fs::read(runtime.join("local-speech-policy.json"))?)?;
        Ok(queued["managed"]["distribution"]
            .as_str()
            .unwrap()
            .to_owned())
    }

    #[test]
    fn changing_both_disk_policies_cannot_replace_a_loading_or_resident_distribution() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        for phase in ["loading", "active"] {
            let mut pin = GpuQueuePin::default();
            pin.admit("Ubuntu", &idle(), &idle())?;
            let captured_new = policies(root.path(), "devbox", true, "qwen_pair_12gb_v1")?;
            assert!(crate::gpu_resource_policy::load(root.path()).is_ok());
            assert!(pin.admit(&captured_new, &idle(), &busy(phase)).is_err());
            assert_eq!(pin.distribution.as_deref(), Some("Ubuntu"));
        }
        Ok(())
    }

    #[test]
    fn disabling_a_disk_peer_or_changing_disk_mode_does_not_release_its_live_pin() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut pin = GpuQueuePin::default();
        pin.admit("Ubuntu", &idle(), &idle())?;
        for mode in ["qwen_pair_12gb_v1", "exclusive"] {
            let captured_new = policies(root.path(), "devbox", false, mode)?;
            assert!(crate::gpu_resource_policy::load(root.path()).is_ok());
            assert!(pin.admit(&captured_new, &idle(), &busy("active")).is_err());
        }
        Ok(())
    }

    #[test]
    fn unconfirmed_cleanup_blocks_admission_and_does_not_clear_the_pin() -> Result<()> {
        let mut pin = GpuQueuePin::default();
        pin.admit("Ubuntu", &idle(), &idle())?;
        for field in ["cleanup_confirmed", "linux_cleanup_confirmed"] {
            let mut unknown = idle();
            unknown[field] = json!(false);
            assert!(pin.admit("devbox", &idle(), &unknown).is_err());
            assert!(pin.admit("Ubuntu", &unknown, &idle()).is_err());
            assert_eq!(pin.distribution.as_deref(), Some("Ubuntu"));
        }
        let failed = json!({"phase":"cleanup_failed","busy":false,"active":false});
        assert!(pin.admit("Ubuntu", &failed, &idle()).is_err());
        Ok(())
    }

    #[test]
    fn a_new_distribution_is_allowed_only_after_both_roles_are_safely_idle() -> Result<()> {
        let mut pin = GpuQueuePin::default();
        pin.admit("Ubuntu", &idle(), &idle())?;
        assert!(pin.admit("devbox", &busy("waiting_gpu"), &idle()).is_err());
        pin.admit("ubuntu", &busy("active"), &idle())?;
        assert_eq!(pin.distribution.as_deref(), Some("Ubuntu"));
        // Stop/finish has no pin API; only this next queue lazily changes it.
        pin.admit("devbox", &idle(), &idle())?;
        assert_eq!(pin.distribution.as_deref(), Some("devbox"));
        Ok(())
    }

    #[test]
    fn a_transitional_status_with_running_false_is_not_cleanup_proof() -> Result<()> {
        let mut pin = GpuQueuePin::default();
        pin.admit("Ubuntu", &idle(), &idle())?;
        for phase in [
            "starting",
            "waiting_gpu",
            "checking",
            "ready_cold",
            "warming",
            "warm_ready_pending_install",
            "active",
            "stopping",
            "cancelled",
        ] {
            let torn = json!({"phase":phase,"busy":false,"active":false});
            assert!(pin.admit("devbox", &torn, &idle()).is_err(), "{phase}");
            assert_eq!(pin.distribution.as_deref(), Some("Ubuntu"));
        }
        Ok(())
    }

    #[test]
    fn simultaneous_queues_validate_and_publish_under_one_pin_lock() {
        for (right, expected_accepts) in [("devbox", 1), ("ubuntu", 2)] {
            let state = Arc::new(Mutex::new((GpuQueuePin::default(), [idle(), idle()])));
            let start = Arc::new(Barrier::new(3));
            let tasks = [(0, "Ubuntu"), (1, right)].map(|(role, captured)| {
                let state = state.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let mut state = state.lock().unwrap();
                    let (pin, snapshots) = &mut *state;
                    if pin.admit(captured, &snapshots[0], &snapshots[1]).is_err() {
                        return false;
                    }
                    snapshots[role] = busy("starting");
                    true
                })
            });
            start.wait();
            let accepted = tasks
                .into_iter()
                .map(|task| task.join().unwrap())
                .filter(|accepted| *accepted)
                .count();
            assert_eq!(accepted, expected_accepts);
        }
    }
}
