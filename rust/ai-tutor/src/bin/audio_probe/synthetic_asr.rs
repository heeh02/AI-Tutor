//! Fixed SAPI-to-file samples; no microphone material or cloud speech calls.
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use windows::{
    Win32::{
        Media::{Audio::WAVEFORMATEX, Speech::*},
        System::Com::*,
    },
    core::{GUID, PCWSTR},
};

struct ComScope;
impl Drop for ComScope {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct TemporaryWav(PathBuf);
impl Drop for TemporaryWav {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) fn synthesize(path: &Path, language: &str, text: &str) -> Result<Vec<f32>> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let _com = ComScope;
        let voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL)?;
        let category: ISpObjectTokenCategory =
            CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL)?;
        category.SetId(SPCAT_VOICES, false)?;
        let filter: Vec<u16> = if language == "zh" {
            "Language=804"
        } else {
            "Language=409"
        }
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let choices = category.EnumTokens(PCWSTR(filter.as_ptr()), PCWSTR::null())?;
        voice.SetVoice(&choices.Item(0)?)?;
        let mut format_id = GUID::default();
        let output = voice.GetOutputStream()?;
        let native_format = output.GetFormat(&mut format_id)?;
        CoTaskMemFree(Some(native_format.cast()));
        drop(output);
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 1,
            nSamplesPerSec: 16000,
            nAvgBytesPerSec: 32000,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let stream: ISpStream = CoCreateInstance(&SpStream, None, CLSCTX_ALL)?;
        let file_name: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        stream.BindToFile(
            PCWSTR(file_name.as_ptr()),
            SPFM_CREATE_ALWAYS,
            Some(&format_id),
            Some(&format),
            0,
        )?;
        voice.SetOutput(&stream, false)?;
        voice.SetSyncSpeakTimeout(15_000)?;
        let words: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        voice.Speak(PCWSTR(words.as_ptr()), SPF_IS_NOT_XML.0 as u32, None)?;
        drop(voice);
        stream.Close()?;
        drop(stream);
    }
    pcm16(path)
}

fn pcm16(path: &Path) -> Result<Vec<f32>> {
    let mut wav = std::fs::read(path)?;
    if wav.len() > 5_000_000 || wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        bail!("SAPI did not produce a bounded WAV file");
    }
    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while offset + 8 <= wav.len() {
        let length = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(length)
            .ok_or_else(|| anyhow!("Invalid WAV chunk"))?;
        if end > wav.len() {
            bail!("Truncated WAV chunk");
        }
        match &wav[offset..offset + 4] {
            b"fmt " if length >= 16 => {
                format = Some((
                    u16::from_le_bytes(wav[start..start + 2].try_into()?),
                    u16::from_le_bytes(wav[start + 2..start + 4].try_into()?),
                    u32::from_le_bytes(wav[start + 4..start + 8].try_into()?),
                    u16::from_le_bytes(wav[start + 14..start + 16].try_into()?),
                ))
            }
            b"data" => data = Some((start, end)),
            _ => {}
        }
        offset = end + (length % 2);
    }
    if format != Some((1, 1, 16000, 16)) {
        bail!("SAPI WAV format was not PCM16 mono 16kHz");
    }
    let (start, end) = data.ok_or_else(|| anyhow!("SAPI WAV has no sample data"))?;
    let samples = wav[start..end]
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
        .collect();
    wav.fill(0);
    Ok(samples)
}

pub fn probe(root: &Path) -> Result<Value> {
    let directory = root.join("runtime/audio-probe-temporary");
    std::fs::create_dir_all(&directory)?;
    let mut options = WhisperContextParameters::default();
    options.use_gpu(false);
    let path = root.join("models/ggml-base.bin");
    let load = Instant::now();
    let model = WhisperContext::new_with_params(
        path.to_str()
            .ok_or_else(|| anyhow!("Model path is invalid"))?,
        options,
    )?;
    let load_ms = load.elapsed().as_millis();
    let mut results = Vec::new();
    for (language, words, terms) in [
        (
            "zh",
            "电机让小车动起来，测距模块告诉主控离墙有多远。停车距离应该大一点。",
            vec!["电机", "测距", "停车"],
        ),
        (
            "en",
            "I want the car to stop before the wall and turn the green light on.",
            vec!["car", "wall", "green"],
        ),
    ] {
        let path = directory.join(format!("{}.wav", uuid::Uuid::new_v4()));
        let mut samples = {
            let _temporary = TemporaryWav(path.clone());
            synthesize(&path, language, words)?
        };
        let removed = !path.exists();
        let sample_count = samples.len();
        let start = Instant::now();
        let mut state = model.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(4);
        params.set_language(Some(language));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_print_special(false);
        params.set_initial_prompt(
            "主控、电机、测距、超声波、停车距离、绿灯。control, motor, distance, light.",
        );
        params.set_abort_callback_safe::<_, Box<dyn FnMut() -> bool>>(Some(Box::new(move || {
            start.elapsed() > Duration::from_secs(45)
        })
            as Box<dyn FnMut() -> bool>));
        let result = state.full(params, &samples);
        samples.fill(0.0);
        result?;
        let mut transcript = String::new();
        for segment in state.as_iter() {
            if segment.no_speech_probability() < 0.6 {
                transcript.push_str(segment.to_str()?);
            }
        }
        let lower = transcript.to_lowercase();
        let matched: Vec<&str> = terms
            .iter()
            .copied()
            .filter(|term| lower.contains(term))
            .collect();
        results.push(json!({"language":language,"source":"fixed local SAPI synthesis to temporary file","speaker_output":false,"input":words,"transcript":transcript.trim(),"sample_count":sample_count,"sample_rate":16000,"model_inference_ms":start.elapsed().as_millis(),"inference_status":if transcript.trim().is_empty(){"FAIL"}else{"PASS"},"content_status":if matched.len()==terms.len(){"PASS"}else{"FAIL"},"matched_terms":matched,"expected_terms":terms,"temporary_wav_removed":removed}));
    }
    let _ = std::fs::remove_dir(&directory);
    Ok(
        json!({"status":if results.iter().all(|r|r["inference_status"]=="PASS"&&r["content_status"]=="PASS"&&r["temporary_wav_removed"]==true){"PASS"}else{"FAIL"},"provider":"native whisper-rs / whisper.cpp / base multilingual / CPU","network_calls":0,"synthetic_samples_only":true,"model_load_ms":load_ms,"results":results}),
    )
}
