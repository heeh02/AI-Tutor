use crate::view::icon;
use yew::prelude::*;

fn active_step(has_learner: bool, has_session: bool, requires_inventory: bool) -> usize {
    if !has_learner {
        0
    } else if !has_session || requires_inventory {
        1
    } else {
        2
    }
}

#[derive(Properties, PartialEq)]
pub struct ExplorationProgressProps {
    pub has_learner: bool,
    pub has_session: bool,
    pub requires_inventory: bool,
    pub ui_language: String,
}

#[function_component(ExplorationProgress)]
pub fn exploration_progress(props: &ExplorationProgressProps) -> Html {
    let en = props.ui_language == "en";
    let active = active_step(
        props.has_learner,
        props.has_session,
        props.requires_inventory,
    );
    let steps = if en {
        [
            ("Choose a profile", "Real or simulated space"),
            ("Confirm modules", "What are we working with today?"),
            ("Start exploring", "Ask, try, observe and save"),
        ]
    } else {
        [
            ("选择匿名档案", "真实记录或独立模拟"),
            ("确认今天的模块", "说清楚现在有什么"),
            ("开始探索", "提问、试验、观察与保存"),
        ]
    };
    html! {
        <nav class="exploration-progress" aria-label={if en { "Getting started" } else { "探索准备进度" }}>
            <ol>
                {for steps.iter().enumerate().map(|(index, (title, detail))| {
                    let complete = index < active;
                    let current = index == active;
                    html! {
                        <li class={classes!(complete.then_some("complete"), current.then_some("current"))}
                            aria-current={if current { "step" } else { "false" }}>
                            <span class="progress-marker">{if complete { icon("check") } else { html! {{(index + 1).to_string()}} }}</span>
                            <div><strong>{*title}</strong><small>{*detail}</small></div>
                        </li>
                    }
                })}
            </ol>
        </nav>
    }
}

#[cfg(test)]
mod tests {
    use super::active_step;

    #[test]
    fn progress_starts_with_profile_and_advances_only_when_ready() {
        assert_eq!(active_step(false, false, false), 0);
        assert_eq!(active_step(true, false, false), 1);
        assert_eq!(active_step(true, true, true), 1);
        assert_eq!(active_step(true, true, false), 2);
    }
}
