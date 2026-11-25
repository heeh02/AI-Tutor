//! Explicit short native-output check. Fixed synthetic speech, no microphone,
//! inference service, cloud request, stored PCM or acoustic-quality assertion.
use super::*;
use sha2::{Digest, Sha256};
use std::{fs, io::Write};

#[test]
#[ignore = "Explicit native output of fixed synthetic English WAV; no microphone, models or cloud"]
fn actual_native_reference_callback() -> Result<()> {
    anyhow::ensure!(
        std::env::var("AI_TUTOR_REAL_RENDER_REFERENCE").as_deref() == Ok("1"),
        "explicit native output opt-in required"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let out = PathBuf::from(std::env::var("AI_TUTOR_RENDER_REFERENCE_REPORT")?);
    anyhow::ensure!(
        out.is_absolute()
            && !out.exists()
            && out
                .parent()
                .unwrap()
                .canonicalize()?
                .starts_with(root.join("runtime/goal-baselines")),
        "new isolated report required"
    );
    let wav = root.join("runtime/goal-baselines/20261103/sapi-corpus/en_short.wav");
    let hash = format!("{:x}", Sha256::digest(fs::read(&wav)?));
    anyhow::ensure!(
        hash == "670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f",
        "fixed synthetic WAV changed"
    );
    let source = voice_loop_source::read_fixed_pcm(&wav)?;
    let pcm: Vec<i16> = resample(&source, 16_000, 24_000)
        .into_iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect();
    let report=std::thread::Builder::new().name("reference-output-probe".into()).spawn(move || {
        let reference=render_reference::RenderReference::new(true);
        let mut reader=reference.reader();
        let mut blocks=Vec::with_capacity(512);
        let mut observed=0usize;let mut nonzero=0usize;let mut unknown=0usize;let mut gaps=0usize;
        let mut result=json!({"passed":false,"source_sha256":hash,"input_source":"fixed_synthetic_wav",
            "microphone_opened":false,"model_calls":0,"cloud_calls":0,"raw_audio_saved":false,
            "acoustic_measurement":false,"device_closed":false});
        let mut collect=|blocks:&[render_reference::ReferenceBlock],state:&render_reference::ReferenceState| {
            gaps+=usize::from(state.discontinuity);
            for block in blocks {
                observed+=block.len;
                nonzero+=block.samples[..block.len].iter().filter(|value|value.abs()>1e-6).count();
                unknown+=usize::from(block.predicted_playback_at.is_none());
            }
        };
        let operation=(||->Result<()> {
            let mut output=pcm_output::PcmOutput::open(Arc::new(AtomicU64::new(1)),1,
                Arc::new(AtomicBool::new(false)),Some(reference.clone()))?;
            let deadline=Instant::now()+Duration::from_secs(10);
            let play=(||->Result<()> {
                let mut position=0;
                loop {
                    anyhow::ensure!(!output.failed() && Instant::now()<deadline,"native output failed or timed out");
                    while position<pcm.len() {
                        let end=(position+streaming_tts::PACKET_SAMPLES).min(pcm.len());
                        if !output.push(&pcm[position..end])? {break;}
                        position=end;
                    }
                    if position==pcm.len() {output.finish_input();}
                    let state=reader.drain_into(&mut blocks,Instant::now());collect(&blocks,&state);
                    if output.drained() {break;}
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(())
            })();
            result["pcm_output"]=json!(output.stop());
            drop(output);
            result["device_closed"]=json!(true);
            let state=reader.drain_into(&mut blocks,Instant::now());collect(&blocks,&state);
            play?;
            Ok(())
        })();
        result["reference"]=reference.status();
        result["observed_reference_samples"]=json!(observed);
        result["nonzero_reference_samples"]=json!(nonzero);
        result["unknown_timestamp_blocks"]=json!(unknown);
        result["reference_gap_observations"]=json!(gaps);
        result["passed"]=json!(operation.is_ok() && observed>0 && nonzero>0 && unknown==0 && gaps==0
            && result["pcm_output"]["predicted_output_drained"]==true);
        result["error"]=if operation.is_ok(){Value::Null}else{json!("native_reference_output_check_failed")};
        result
    })?.join().map_err(|_|anyhow!("native reference owner panicked"))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    anyhow::ensure!(
        report["passed"] == true,
        "native reference check did not pass; inspect report"
    );
    Ok(())
}
