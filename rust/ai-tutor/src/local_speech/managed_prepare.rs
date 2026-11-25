//! Shared managed TTS preparation, independent of Workstation and audio devices.
use super::{
    LocalSpeech, ManagedSpeechConfig, PreparedTts,
    managed::{OwnedManagedTts, engine_endpoint},
};
use crate::audio::streaming_tts::{TtsClient, wait_cancelled};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::json;
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

pub(crate) fn managed_client(
    root: &std::path::Path,
    config: &ManagedSpeechConfig,
    token: &str,
) -> Result<Arc<TtsClient>> {
    let (endpoint, model) = engine_endpoint(&config.engine)?;
    let mut value =
        json!({"protocol_version":1,"endpoint":endpoint,"model":model,"bearer_token":token});
    let mut directory = root.to_owned();
    if let Some(file) = &config.references_file {
        let file = root.join(file).canonicalize()?;
        let mut bytes = Vec::new();
        File::open(&file)?
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 16 * 1024,
            "Bilingual voice reference configuration exceeds its bound"
        );
        value["references"] = serde_json::from_slice(&bytes)?;
        directory = file.parent().unwrap().to_owned();
    }
    Ok(Arc::new(TtsClient::from_configuration(
        &serde_json::to_vec(&value)?,
        &directory,
    )?))
}

/// The caller retains `owned` on every error and must await its stop receipt.
pub(crate) async fn prepare_owned_managed(
    root: &Path,
    local: &LocalSpeech,
    owned: &mut Option<OwnedManagedTts>,
    cancellation: &AtomicU64,
    epoch: u64,
    mut progress: impl FnMut(),
) -> Result<PreparedTts> {
    let config = local.policy().managed.as_ref().unwrap();
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let instance = uuid::Uuid::new_v4().simple().to_string();
    // Validate local reference files before acquiring a GPU lease.
    let client = managed_client(root, config, &token)?;
    local.update_lifecycle(
        json!({"phase":"starting","ready":false,"active":false,"instance_id":instance}),
    );
    *owned =
        Some(OwnedManagedTts::launch(root, config, &token, &instance, cancellation, epoch).await?);
    let service = owned.as_mut().unwrap();
    service
        .wait_for_starting(cancellation, epoch, |event| {
            local.update_lifecycle(json!({"phase":if event["state"] == "waiting_gpu" {
            "waiting_gpu"
        } else { "starting" }}));
            progress();
        })
        .await?;
    // Fixed-KV cold startup measured 249.82 s. Queue waiting is separate;
    // the warm first-PCM and acoustic latency goals are unchanged.
    let readiness_budget = Duration::from_secs(300);
    let readiness = async {
        let deadline = tokio::time::Instant::now() + readiness_budget;
        loop {
            if service.poll_running()? && client.check_ready().await.is_ok() {
                return Ok::<_, anyhow::Error>(());
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "Managed readiness deadline exceeded"
            );
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    };
    tokio::select! { biased;
        _=wait_cancelled(cancellation,epoch)=>bail!("Local speech startup cancelled"),
        result=tokio::time::timeout(readiness_budget,readiness)=>result.map_err(|_|anyhow!("Managed readiness deadline exceeded"))??,
    }
    let starting = service.starting_receipt().cloned();
    let prepared = tokio::select! { biased;
        _=wait_cancelled(cancellation,epoch)=>bail!("Local speech warmup cancelled"),
        result=local.prepare_managed_tts(client)=>result?,
    };
    tokio::select! { biased;
        _=wait_cancelled(cancellation,epoch)=>bail!("Local speech warmup cancelled"),
        result=service.finish_preparation()=>result?,
    }
    local.update_lifecycle(json!({"mode":"managed","instance_id":instance,
        "ownership_start":starting,"managed_instance_owned":true}));
    Ok(prepared)
}
