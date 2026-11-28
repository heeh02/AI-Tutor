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

pub const ACTIVITIES: [CompanionActivity; 6] = [
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
        title_zh: "好奇问答",
        title_en: "Curious questions",
        description_zh: "自然、宇宙与日常生活",
        description_en: "Nature, space and everyday life",
        prompt_zh: "我有很多“为什么”想问。我们先从“月亮为什么看起来会跟着我走”开始，请用生活中的例子和我一起想，不要只给定义。",
        prompt_en: "I have lots of questions. Let's start with why the moon seems to follow me. Think it through with me using an everyday example, not just a definition.",
    },
    CompanionActivity {
        id: "learning",
        icon: "study",
        title_zh: "学习辅导",
        title_en: "Learning support",
        description_zh: "分步理解问题与练习",
        description_en: "Understand problems one step at a time",
        prompt_zh: "我有一道不会的题。请先让我把题目和已经试过的方法告诉你，再一步一步引导我思考，不要直接替我完成。",
        prompt_en: "I have a question I cannot solve. Let me first tell you the problem and what I have already tried. Then guide my thinking one step at a time instead of completing it for me.",
    },
    CompanionActivity {
        id: "creative",
        icon: "palette",
        title_zh: "创意活动",
        title_en: "Creative activities",
        description_zh: "绘画、设计与安全手工",
        description_en: "Drawing, design and safe crafts",
        prompt_zh: "我想做一个有趣的小创作。请先问我身边有哪些安全的材料，再陪我选一个能动手完成的作品，一次只做一个步骤。",
        prompt_en: "I want to make something creative. First ask me what safe materials I have nearby, then help me choose a project I can make, one simple step at a time.",
    },
    CompanionActivity {
        id: "english",
        icon: "chat",
        title_zh: "英语小游戏",
        title_en: "English play",
        description_zh: "单词、猜词与口语练习",
        description_en: "Vocabulary, guessing and speaking practice",
        prompt_zh: "我们玩一个简单的英语猜词游戏吧。请用简单英语配合中文提示，一次只给我一个线索，等我回答后再继续。",
        prompt_en: "Let's play a simple English guessing game. Use easy English with a little Chinese help. Give me one clue at a time and wait for my answer before continuing.",
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
    fn six_general_companion_activities_have_distinct_bilingual_content() {
        let ids: HashSet<_> = ACTIVITIES.iter().map(|activity| activity.id).collect();
        assert_eq!(ids.len(), 6);
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
