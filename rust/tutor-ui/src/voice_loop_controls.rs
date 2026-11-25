use crate::{
    view::SelectControl,
    voice_loop::{Device, Phase},
};
use serde_json::Value;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub language: String,
    pub phase: Phase,
    pub pause_reason: String,
    pub available: bool,
    pub can_begin: bool,
    pub selected: String,
    pub devices: Vec<Value>,
    pub barge_in_available: bool,
    pub barge_in_requested: bool,
    pub barge_in_active: bool,
    pub monitoring: bool,
    pub on_device: Callback<String>,
    pub on_begin: Callback<MouseEvent>,
    pub on_pause: Callback<MouseEvent>,
    pub on_barge_in: Callback<Event>,
}
#[function_component(VoiceLoopControls)]
pub fn voice_loop_controls(props: &Props) -> Html {
    let en = props.language == "en";
    let active = !matches!(props.phase, Phase::Off | Phase::Paused);
    let label = match props.phase {
        Phase::Off => {
            if en {
                "Manual review and send"
            } else {
                "手动检查后发送"
            }
        }
        Phase::Paused => {
            if en {
                "Paused · continue explicitly"
            } else {
                "已暂停 · 需明确继续"
            }
        }
        Phase::Enabling | Phase::Armed | Phase::Starting => {
            if en {
                "Preparing to listen"
            } else {
                "正在准备听取"
            }
        }
        Phase::Listening => {
            if en {
                "Listening · silence can end your turn"
            } else {
                "正在听取 · 停顿可能结束本轮"
            }
        }
        Phase::Finalizing | Phase::ReadyToSubmit => {
            if en {
                "Preparing your transcript"
            } else {
                "正在整理本次转写"
            }
        }
        Phase::Submitting => {
            if en {
                "Sending this turn once"
            } else {
                "正在单次发送本轮"
            }
        }
        Phase::Reply => {
            if props.barge_in_active && props.monitoring {
                if en {
                    "Listening for your interruption during the reply"
                } else {
                    "正在回复，同时听取你的打断"
                }
            } else if props.barge_in_active {
                if en {
                    "Preparing interruption listening"
                } else {
                    "正在准备打断监听"
                }
            } else if en {
                "Waiting for the whole reply to finish speaking"
            } else {
                "等待整条回复播完"
            }
        }
    };
    let on_device = props.on_device.clone();
    html! {
        <section class="live-transcript voice-loop-controls" aria-label={if en {"Experimental voice conversation"} else {"实验性语音轮换"}}>
            <strong>{if en {"Take turns speaking · experimental"} else {"轮流语音对话 · 实验功能"}}</strong>
            <p aria-live="polite">{label}</p>
            {if props.phase == Phase::Paused {html!{<small>{match props.pause_reason.as_str() {
                "edited" => if en {"Your edits are retained. No automatic send or listening will resume."} else {"已保留你的编辑，不会自动发送或重新听取。"},
                "submission_unknown" => if en {"Check the conversation and the pending-send card before deciding whether to send again."} else {"请先核对对话记录与待核对发送卡，再决定是否另行发送。"},
                "duration_limit" | "transcript_unavailable" => if en {"This recording did not qualify for automatic sending. Review any retained transcript manually."} else {"本次录音不满足自动发送条件；如有保留转写，请人工检查。"},
                "network" | "permission_lost" | "enable_failed" | "start_failed" => if en {"Voice conversation could not continue. Check the current status, then choose Continue."} else {"语音对话未能继续，请检查当前状态后再选择继续。"},
                _ => if en {"Listening will resume only after you choose Start / continue again."} else {"只有再次选择“开始 / 继续”，才会重新听取。"},
            }}</small>}} else {Html::default()}}
            <p>{if en {"Start allows this session to listen, send an untouched final transcript, speak the full reply, and listen again. Typing, hiding this page, or stopping pauses the loop. It never sends again after an uncertain request."} else {"开始即允许当前活动依次听取、发送未编辑的终稿、播完整条回复后再听。编辑、隐藏页面或停止都会暂停；请求结果不明时不会重发。"}}</p>
            <p>{if en {"Not validated for classroom use: a 2.5-second thinking pause can cut a turn short; real speaker echo remains unverified. Stop and Escape always remain available."} else {"尚未通过课堂验收：2.5 秒思考停顿可能被提前截断，真实扬声器回声仍未验证。随时可以按“停止”或 Esc。"}}</p>
            <label><input type="checkbox" checked={props.barge_in_requested}
                disabled={active||(!props.barge_in_available&&!props.barge_in_requested)} onchange={props.on_barge_in.clone()}/>
                {if en {"Allow speech interruption · experimental"} else {"允许说话打断 · 实验功能"}}
            </label>
            <small>{if props.barge_in_requested {
                if en {"Keeps this microphone open during replies. Echo may cause a false interruption; Pause closes listening."} else {"回复期间保持本次麦克风开启。回声可能造成误判；暂停会关闭监听。"}
            } else if !props.barge_in_available {
                if en {"Speech interruption requires local echo processing and normal-speed enhanced speech. Otherwise use Stop or Escape."} else {"说话打断需要本地回声处理及正常语速的增强语音。未启用时请用停止或 Esc。"}
            } else {
                if en {"Off by default. Without it, speaking over playback does not interrupt the reply."} else {"默认关闭；未勾选时，播报中说话不会打断回复。"}
            }}</small>
            <label>{if en {"Microphone for this conversation"} else {"本次使用的麦克风"}}
                <SelectControl value={props.selected.clone()} label={if en {"Microphone for voice conversation"} else {"语音轮换麦克风"}} disabled={active} onchange={Callback::from(move |event:Event| {
                    let input=event.target_unchecked_into::<web_sys::HtmlSelectElement>();on_device.emit(input.value());
                })}>
                    <option value="">{if en {"Select a microphone"} else {"请选择麦克风"}}</option>
                    {for props.devices.iter().filter(|device|Device::parse(device).is_some()).map(|device|html!{
                        <option value={device["device_key"].as_str().unwrap_or_default().to_owned()}>
                            {device["name"].as_str().unwrap_or(if en {"Microphone"} else {"麦克风"})}
                        </option>
                    })}
                </SelectControl>
            </label>
            {if active {html!{<button class="stop-button" onclick={props.on_pause.clone()}>{if en {"Pause voice conversation"} else {"暂停语音轮换"}}</button>}}
            else {html!{<button class="text-button" disabled={!props.can_begin} onclick={props.on_begin.clone()}>{if en {"Start / continue voice conversation"} else {"开始 / 继续语音轮换"}}</button>}}}
            {if !props.available {html!{<small>{if en {"Voice conversation is not enabled on this workstation. You can still type or record manually."} else {"此工作台尚未启用轮流语音对话，仍可输入文字或手动录音。"}}</small>}}
            else if !active && !props.can_begin {html!{<small>{if en {"Select a microphone, enable next-send auto-read, and resolve drafts or pending results before starting."} else {"开始前请选择麦克风、勾选下次发送自动朗读，并处理草稿和待核对结果。"}}</small>}}
            else {Html::default()}}
        </section>
    }
}
