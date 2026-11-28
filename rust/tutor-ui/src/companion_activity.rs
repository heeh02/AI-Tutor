pub struct CompanionActivity {
    pub id: &'static str,
    pub icon: &'static str,
    pub title_zh: &'static str,
    pub title_en: &'static str,
    pub description_zh: &'static str,
    pub description_en: &'static str,
    pub prompt_zh: &'static str,
    pub prompt_en: &'static str,
}

impl CompanionActivity {
    pub fn prompt(&self, english: bool) -> &'static str {
        if english {
            self.prompt_en
        } else {
            self.prompt_zh
        }
    }
}

pub const ACTIVITIES: [CompanionActivity; 4] = [
    CompanionActivity {
        id: "story",
        icon: "book",
        title_zh: "互动故事",
        title_en: "Interactive stories",
        description_zh: "原创故事与情节选择",
        description_en: "Original stories with choices",
        prompt_zh: "我想和你一起编一个互动故事。请先设定一个有趣但不吓人的主角和场景，每次讲一小段，再给我一个简单的情节选择。",
        prompt_en: "Let's create an interactive story together. Start with an interesting, non-scary character and setting. Tell one short part at a time, then give me one simple choice about what happens next.",
    },
    CompanionActivity {
        id: "curiosity",
        icon: "light",
        title_zh: "问为什么",
        title_en: "Ask why",
        description_zh: "学习、自然、宇宙与日常生活",
        description_en: "Learning, nature, space and everyday life",
        prompt_zh: "我有很多“为什么”想问。我们先从“月亮为什么看起来会跟着我走”开始，请用生活中的例子和我一起想，不要只给定义。",
        prompt_en: "I have lots of questions. Let's start with why the moon seems to follow me. Think it through with me using an everyday example, not just a definition.",
    },
    CompanionActivity {
        id: "play",
        icon: "palette",
        title_zh: "一起玩",
        title_en: "Play together",
        description_zh: "绘画、手工与语言游戏",
        description_en: "Drawing, crafts and word games",
        prompt_zh: "我想做一个有趣的小创作或玩一个英语猜词游戏。请先问我身边有哪些安全的材料，再陪我选一个能动手完成的作品或游戏，一次只做一个步骤。",
        prompt_en: "I want to make something creative or play an English guessing game. First ask me what safe materials I have nearby, then help me choose one project or game, one simple step at a time.",
    },
    CompanionActivity {
        id: "feelings",
        icon: "heart",
        title_zh: "情绪表达",
        title_en: "Feelings and experiences",
        description_zh: "分享经历与感受",
        description_en: "Talk about experiences and feelings",
        prompt_zh: "我想和你说说今天发生的一件事。请先听我把事情和感受讲完，不要急着评价、给建议或替我下结论。",
        prompt_en: "I want to tell you about something that happened today. Please listen to what happened and how I feel before offering advice or drawing conclusions.",
    },
];

pub fn selected_activity(input: &str, english: bool) -> Option<&'static str> {
    ACTIVITIES
        .iter()
        .find(|activity| activity.prompt(english) == input)
        .map(|activity| activity.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn four_general_companion_activities_have_distinct_bilingual_content() {
        let ids: HashSet<_> = ACTIVITIES.iter().map(|activity| activity.id).collect();
        assert_eq!(ids, ["story", "curiosity", "play", "feelings"].into_iter().collect());
        for activity in ACTIVITIES {
            assert!(!activity.title_zh.is_empty() && !activity.title_en.is_empty());
            assert!(!activity.description_zh.is_empty() && !activity.description_en.is_empty());
            assert!(!activity.prompt(false).is_empty() && !activity.prompt(true).is_empty());
            assert!(!activity.prompt_zh.contains("小车"));
            assert!(!activity.prompt_zh.contains("模块"));
        }
    }

    #[test]
    fn selection_only_survives_an_unchanged_prompt() {
        let prompt = ACTIVITIES[0].prompt(false);
        assert_eq!(selected_activity(prompt, false), Some("story"));
        assert_eq!(selected_activity(prompt, true), None);
        assert_eq!(selected_activity("我自己的草稿", false), None);
    }
}
