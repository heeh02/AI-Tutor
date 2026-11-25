use crate::recording_draft::ReviewKind;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RecordingReviewProps {
    pub transcript: Option<String>,
    pub submission: Option<String>,
    pub ui_language: String,
    pub enabled: bool,
    pub on_adopt: Callback<(ReviewKind, bool)>,
    pub on_dismiss: Callback<ReviewKind>,
}

#[function_component(RecordingReview)]
pub fn recording_review(props: &RecordingReviewProps) -> Html {
    let en = props.ui_language == "en";
    html! {<>
        {for [(ReviewKind::Transcript,props.transcript.as_ref()),(ReviewKind::Submission,props.submission.as_ref())]
            .into_iter().filter_map(|(kind,text)|text.map(|text| {
                let replace=props.on_adopt.clone();
                let append=props.on_adopt.clone();
                let dismiss=props.on_dismiss.clone();
                html!{<section class="recording-review" aria-label={match(kind,en){
                    (ReviewKind::Transcript,false)=>"未采用转写",(ReviewKind::Transcript,true)=>"Transcript waiting for review",
                    (ReviewKind::Submission,false)=>"发送结果待确认",(ReviewKind::Submission,true)=>"Unconfirmed send result"}}>
                    <strong>{match(kind,en){
                        (ReviewKind::Transcript,false)=>"未采用转写",(ReviewKind::Transcript,true)=>"Transcript waiting for review",
                        (ReviewKind::Submission,false)=>"发送结果待确认",(ReviewKind::Submission,true)=>"Unconfirmed send result"}}</strong>
                    <p>{match(kind,en){
                        (ReviewKind::Transcript,false)=>"转写尚未写入输入框。采用后仍需手动发送；处理后可开始下一次录音。",
                        (ReviewKind::Transcript,true)=>"This transcript has not changed your draft. Review it before recording again; sending remains manual.",
                        (ReviewKind::Submission,false)=>"请先查看当前消息，确认是否已经发送。此文本不会自动重发；处理后才能再次发送。",
                        (ReviewKind::Submission,true)=>"Check the current messages to see whether this was sent. It will not be resent automatically; resolve it before sending again."}}</p>
                    <p class="recording-review-text">{text}</p>
                    <div><button class="text-button" disabled={!props.enabled} onclick={Callback::from(move |_|replace.emit((kind,false)))}>{if en{"Replace draft"}else{"替换输入框"}}</button>
                    <button class="text-button" disabled={!props.enabled} onclick={Callback::from(move |_|append.emit((kind,true)))}>{if en{"Append to draft"}else{"追加到草稿"}}</button>
                    <button class="text-button" onclick={Callback::from(move |_|dismiss.emit(kind))}>{if en{"Dismiss this text"}else{"忽略这段文字"}}</button></div>
                </section>}
            }))}
    </>}
}
