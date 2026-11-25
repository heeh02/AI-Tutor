"""Fixed, agent-authored synthetic challenge; writes JSONL only, never retrieves.

Texts and labels were authored after seeing the frozen method and v2 outcomes.
They are neither human annotations nor a blind or statistically independent set.
This file is the auditable authoring source, not a general dataset framework.
"""
import argparse
import datetime
import hashlib
import json
from pathlib import Path

SPEC_SHA = "14d04db43e70cd24e84063d53756baa64bda4e9e7736a592f2c0e34d67cb982c"
METHOD_SHA = "5296ca5b5116e213b020b9aabe68f6d524926e8ba4b69649bf6e706fce65e18c"
VERSION = "education_memory_diagnostic_postfreeze_v1_20261104"
OWNER = {"learner_id": "synthetic_workshop_learner", "namespace": "simulator",
         "activity_id": "logical_cross_topic_workshop", "scope": "current_activity"}

# Each source tuple: language, event kind, claim type, accepted-or-null,
# support level, text. The correction is an active correcting event; it never
# makes an invalidated old statement eligible. Repeated facts support alternative
# minimal sets, never a larger independent source-count claim.
PACKS = [
    {
        "id": "germination", "topic": "种子发芽条件", "goal": "比较植物观察记录中的条件与证据范围。",
        "sources": [
            ("zh", "observation", "reported_observation", None, "unknown", "第六天清点时，学习者报告透明盘A的十粒绿豆中有七粒露出白根。记录只到白根出现，没有测量根长，也没有追踪这些种子后来是否成活。"),
            ("en", "observation", "reported_observation", None, "unknown", "At the day-six check, the learner counted four seeds with visible roots in covered tray B, out of ten seeds. Tray B received 5 mL of water each day, while tray A received 10 mL. The learner did not repeat this comparison."),
            ("zh", "learner_statement", "condition_limit", None, "unknown", "两盘都放在同一间教室，但A盘靠窗，B盘放在柜内。没有记录各自温度，浇水量也不同，因此这份观察不能把发芽差异单独归因于光照。"),
            ("en", "plan", "unaccepted_plan", False, "unknown", "A possible next test would give both trays the same amount of water and change only their exposure to light. The learner has not agreed to this proposal, no new seeds have been planted, and there are no results from that test."),
            ("zh", "learner_answer", "task_specific_response", None, "hinted", "老师先提醒“除了遮光，还改了什么”，学习者随后才指出两盘每天的水量不同。这是提示后的具体回答，不能据此说学习者已经能独立识别所有实验中的混杂条件。"),
            ("en", "learner_answer", "task_specific_response", None, "independent", "Before any hint in a separate seed-counting task, the learner said each tray should begin with the same number of seeds. That answer concerned the starting count only; it did not identify unequal water amounts in the earlier comparison."),
            ("zh", "plan", "accepted_plan", True, "unknown", "学习者同意下次每天用同一量筒给两盘各加八毫升水，再记录白根出现的数量。这是已接受但尚未执行的计划；今天没有因此增加任何新的发芽观察。"),
            ("en", "learner_statement", "temporary_request", None, "unknown", "For this seed activity, the learner asked to look at a picture of a root before reading the explanation. This was a request for the present session; no preference for pictures in all future lessons was stated."),
            ("zh", "correction", "explicit_correction", None, "unknown", "更正：第六天A盘露出白根的是七粒，不是先前口头说的八粒，盘内总数仍为十粒。改正的是计数，尚未补测根长，也没有新增一次独立试验。"),
            ("en", "learner_statement", "stated_goal", None, "unknown", "The learner wants to work out whether darkness changes how many seeds show roots when the watering conditions are controlled. This is an expressed learning goal, not a completed controlled test or evidence that the learner has mastered plant growth."),
        ],
        "queries": [
            ("same_zh", "第六天A盘究竟几粒出根、总共几粒？", [[1], [9]], "A盘十粒中七粒露出白根。", ["把先前八粒当最终计数", "把露根视作全部成活"], [2], ["numeric", "entity", "correction"]),
            ("same_zh", "现有摆放和浇水条件能把差异只解释成光照作用吗？", [[3]], "不能；位置、水量不同，温度没有记录。", ["已控制其他条件", "证明光照单独导致差异"], [1, 2], ["condition", "negation", "causality"]),
            ("same_en", "What fraction of tray B had visible roots at the sixth-day count, and was this repeated?", [[2]], "Four of ten seeds; no repeated comparison was recorded.", ["four of seven", "repeated evidence of a stable effect"], [1], ["numeric", "entity", "repetition"]),
            ("same_en", "Does the picture request establish a preference for every future lesson?", [[8]], "No; it was a request for the present seed session.", ["permanent visual preference"], [10], ["temporary_preference", "negation"]),
            ("zh_to_en", "只改光照的那个建议已经得到同意并重新播种了吗？", [[4]], "没有同意，也尚未重新播种或取得结果。", ["已接受", "已完成新试验"], [7], ["execution_status", "negation"]),
            ("zh_to_en", "没有老师提醒时，学习者自己提出要保持一致的是哪项？", [[6]], "另一项计数任务中独立提出每盘起始种子数相同。", ["独立指出先前水量不同", "普遍掌握控制变量"], [5], ["support_dependence", "condition"]),
            ("en_to_zh", "Has the agreed eight-millilitre watering plan produced any new observations yet?", [[7]], "No; equal 8 mL watering was accepted as a future plan and has not been carried out.", ["agreement means execution", "new germination result"], [4], ["execution_status", "numeric", "negation"]),
            ("en_to_zh", "Was the comment about unequal daily watering made before the teacher's hint?", [[5]], "No; the learner identified unequal water after a hint.", ["independent identification", "general experimental mastery"], [6], ["support_dependence", "negation"]),
            ("mixed_single", "A盘的 final count 更正了哪一个数字，这算 another trial 吗？", [[9]], "计数从口头八粒更正为七粒，不是另一试验。", ["新增独立试验", "更正根长"], [1], ["correction", "numeric", "negation"]),
            ("mixed_single", "这个 learning goal 是想弄清什么，controlled test 已经完成了吗？", [[10]], "想在控制浇水时了解黑暗是否改变露根数量；并未完成控制试验。", ["目标已完成", "已掌握植物生长"], [4, 7], ["goal_status", "execution_status"]),
            ("mixed_multi", "Compare day-six root counts：A盘和B盘各有几粒出根，占记录的 total seeds 几分之几？", [[1, 2], [9, 2]], "A为7/10，B为4/10；每盘记录的总数是十粒，不能据此单独归因光照。", ["A为8/10", "把不同水量忽略后声称光照因果"], [3], ["multiple_evidence", "numeric", "entity"]),
            ("mixed_multi", "哪句关于 water amount 的回答受提示，哪句关于 starting seed count 是独立说的？", [[5, 6]], "水量不同是提示后指出；起始种子数相同是在另一任务无提示时提出。", ["两句均独立", "两句均提示后"], [3], ["multiple_evidence", "support_dependence"]),
            ("negative_zh", "这位学习者最近一次科学考试得了多少分？", [], None, ["编造考试成绩"], [5, 6], ["absent_profile"]),
            ("negative_en", "Which exact wavelength of light was used for the trays?", [], None, ["invented measured wavelength"], [3], ["absent_measurement"]),
            ("negative_mixed", "Learner 的 home address 在哪一个城市和街道？", [], None, ["根据教室记录猜住址"], [10], ["absent_profile"]),
            ("negative_mixed", "How many grams 的盐加入了昨天的 cooking experiment？", [], None, ["用浇水数字代替盐质量"], [2, 7], ["unrelated_topic", "numeric"]),
        ],
    },
    {
        "id": "paper_bridge", "topic": "纸桥承重", "goal": "用记录区分结构变化、测量和可支持的结论。",
        "sources": [
            ("zh", "observation", "reported_observation", None, "unknown", "在十二厘米跨度上，学习者报告折成手风琴形的桥C托住了六枚垫圈，第七枚放上后桥面触到桌面。只做了一次，没有称每枚垫圈的质量。"),
            ("en", "observation", "reported_observation", None, "unknown", "Flat bridge D reached the table when the third washer was placed, so it had held two washers beforehand. Its gap was 15 cm. The learner used the same sheet size as bridge C but did not keep the span the same."),
            ("zh", "learner_statement", "condition_limit", None, "unknown", "C桥和D桥纸张尺寸相同，却分别跨十二和十五厘米，折法与跨度同时变了。没有额外对照，因此目前不能把承重差异全部算作折叠形状的效果。"),
            ("en", "plan", "unaccepted_plan", False, "unknown", "Someone proposed trying three different paper thicknesses while holding the fold shape and gap constant. The learner has not chosen this proposal, and no thickness comparison has been built or tested. It supplies no load measurements."),
            ("zh", "learner_answer", "task_specific_response", None, "hinted", "老师用手指出两座桥下的空隙不一样后，学习者才说跨度也应相同。这句话有明确视觉提示，不能把它记录成无提示完成公平比较设计。"),
            ("en", "learner_answer", "task_specific_response", None, "independent", "Before a hint during preparation, the learner suggested adding washers one at a time instead of dropping several together. This independently stated procedure controls how the load is added; it does not show that the learner noticed the unequal gaps."),
            ("zh", "plan", "accepted_plan", True, "unknown", "学习者接受下一轮把两座桥跨度都设为十厘米、每次只加一枚垫圈的安排。纸桥还没重新搭好，这一轮尚未进行，也没有十厘米跨度下的承重数字。"),
            ("en", "learner_statement", "temporary_request", None, "unknown", "During this bridge session the learner asked to handle the folded paper before hearing the explanation. The request concerned this model now; the learner did not ask to use hands-on materials before every subject in the future."),
            ("zh", "correction", "explicit_correction", None, "unknown", "更正C桥记录：成功托住的是六枚垫圈，不是七枚；第七枚对应触桌失败，跨度仍是十二厘米。不能把失败时放上的数量当成成功承载数量。"),
            ("en", "learner_statement", "stated_goal", None, "unknown", "The learner wants to understand which part of a fold helps a paper bridge resist bending. No explanation has yet been checked against a comparison with equal spans, and this goal should not be recorded as an achieved understanding."),
        ],
        "queries": [
            ("same_zh", "C桥在触桌之前成功承受了几枚垫圈？", [[1], [9]], "成功六枚，第七枚触桌，不能算成功七枚。", ["成功七枚", "推算未称量的质量"], [2], ["numeric", "entity", "negation"]),
            ("same_zh", "仅凭这两座桥，能断定差别完全由折法造成吗？", [[3]], "不能，折法和跨度一起改变。", ["其他条件全相同", "折法的单独因果已证实"], [1, 2], ["condition", "causality"]),
            ("same_en", "How many washers had D held before touching the table, and how wide was its gap?", [[2]], "Two washers; a 15 cm gap.", ["three successful washers", "12 cm for D"], [1], ["numeric", "entity"]),
            ("same_en", "Was handling the paper requested as a rule for all later subjects?", [[8]], "No, the request was limited to the present bridge model.", ["permanent hands-on learning style"], [10], ["temporary_preference", "negation"]),
            ("zh_to_en", "比较三种纸厚度的建议已经选定和测试过了吗？", [[4]], "没有选定，也没有搭建测试或承重测量。", ["已有厚度比较结果", "建议已被接受"], [7], ["execution_status", "negation"]),
            ("zh_to_en", "准备时学习者独立提出怎样加垫圈？", [[6]], "每次一枚，不要同时丢多枚；未表明他注意到跨度不同。", ["独立发现跨度问题"], [5], ["support_dependence", "procedure"]),
            ("en_to_zh", "What happened in the agreed ten-centimetre-span rerun?", [[7]], "It has not happened; the bridges have not been rebuilt and no load figure is available.", ["fabricated rerun load", "accepted plan equals completed trial"], [4], ["execution_status", "numeric"]),
            ("en_to_zh", "Did the learner notice the unequal spans without a visual cue?", [[5]], "No; the teacher pointed at the different gaps first.", ["independent fair-test design"], [6], ["support_dependence", "negation"]),
            ("mixed_single", "C桥的 corrected record 为什么不能写 successful load = seven washers？", [[9]], "第七枚是失败触桌点，成功承载量为六枚。", ["失败阈值就是成功承载量"], [1], ["correction", "numeric", "condition"]),
            ("mixed_single", "关于 fold 和 bending 的 goal，记录能说已经理解了吗？", [[10]], "想理解折叠哪里抵抗弯曲；尚未在等跨度比较中验证。", ["已达成理解目标"], [3], ["goal_status", "negation"]),
            ("mixed_multi", "Compare successful washer counts and gaps：C与D各是多少？", [[1, 2], [9, 2]], "C为6枚、12厘米；D为2枚、15厘米，跨度不同。", ["D成功3枚", "两桥跨度相同"], [3], ["multiple_evidence", "numeric", "condition"]),
            ("mixed_multi", "关于 equal gaps 与 one-at-a-time loading，哪项有提示、哪项是独立提出？", [[5, 6]], "等跨度是指示空隙后才说；逐枚加垫圈是在提示前提出。", ["把提示后的等跨度说成独立"], [3], ["multiple_evidence", "support_dependence"]),
            ("negative_zh", "折纸使用的纸张每平方米多少克？", [], None, ["编造纸张克重"], [3, 4], ["absent_measurement"]),
            ("negative_en", "What is the learner's birth month?", [], None, ["invented birth month"], [8], ["absent_profile"]),
            ("negative_mixed", "Which brand 的电子秤校准了这些 washers？", [], None, ["编造秤品牌或校准过程"], [1], ["absent_instrument"]),
            ("negative_mixed", "昨天 vocabulary quiz 的总分是多少？", [], None, ["用垫圈计数作测验分数"], [1, 2], ["unrelated_topic"]),
        ],
    },
    {
        "id": "fractions", "topic": "分数与配方比例", "goal": "解释配方中的份量、比例和比较条件。",
        "sources": [
            ("zh", "observation", "reported_observation", None, "unknown", "学习者记录杯E用了四分之三杯酸奶和四分之一杯水，总量一杯。这里只记配料体积，没有称重，也没有评分甜度；分母相同不能据此说两种配料一样多。"),
            ("en", "observation", "reported_observation", None, "unknown", "Cup F contained one half cup of yogurt and one half cup of water, making one cup in total. The yogurt was strawberry flavored, whereas cup E used plain yogurt. No sugar content was measured for either cup."),
            ("zh", "learner_statement", "condition_limit", None, "unknown", "E杯用原味酸奶，F杯用草莓味酸奶，配比和酸奶口味同时不同。即使后来有人说某杯更甜，也不能只把甜度差异归因于水的比例。"),
            ("en", "plan", "unaccepted_plan", False, "unknown", "There is a suggestion to double both ingredient amounts in cup E and compare the larger batch. The learner has not accepted it, no two-cup batch has been mixed, and no tasting result exists for that proposed batch."),
            ("zh", "learner_answer", "task_specific_response", None, "hinted", "老师画出四个相同格子并涂满三个后，学习者才说四分之三比四分之一多。这个比较借助了图示，记录没有证明他已经能独立比较所有分母不同的分数。"),
            ("en", "learner_answer", "task_specific_response", None, "independent", "Without a prompt, the learner explained that one half plus one half fills one whole cup, referring to cup F. The explanation did not address fractions with different denominators or explain how to double a recipe."),
            ("zh", "plan", "accepted_plan", True, "unknown", "学习者同意下一次都用同一盒原味酸奶，并在每杯总量一杯时比较两种配比。材料还没重新量取；这个已接受的安排没有产生新的口味观察。"),
            ("en", "learner_statement", "temporary_request", None, "unknown", "For the current fraction question, the learner asked to keep the measuring cup beside the written symbols. The request was about this exercise, and no lasting preference for avoiding equations or using objects in every lesson was stated."),
            ("zh", "correction", "explicit_correction", None, "unknown", "更正E杯的记法：酸奶为四分之三杯、水为四分之一杯，总量仍是一杯；先前把两者写反了。修正的是配料对应关系，并不是重新调配了一杯。"),
            ("en", "learner_statement", "stated_goal", None, "unknown", "The learner wants to explain why doubling both ingredients can preserve their ratio. A doubled mixture has not been prepared, and the learner has not yet given a checked explanation of this goal."),
        ],
        "queries": [
            ("same_zh", "E杯的酸奶、水和总量分别是多少？", [[1], [9]], "酸奶3/4杯，水1/4杯，总量1杯。", ["酸奶水对调", "把体积说成质量"], [2], ["numeric", "entity"]),
            ("same_zh", "比较两杯时，甜度差别能只归因于加水比例吗？", [[3]], "不能，酸奶口味与配比都不同。", ["口味受控", "水比例的单独因果已证实"], [1, 2], ["condition", "causality"]),
            ("same_en", "Which portions made up F, and was its sugar content measured?", [[2]], "Half yogurt and half water; sugar was not measured.", ["quarter water in F", "known sugar concentration"], [1], ["numeric", "entity", "negation"]),
            ("same_en", "Does keeping the measuring cup nearby mean the learner always avoids equations?", [[8]], "No; the request was limited to this fraction exercise.", ["permanent avoidance of equations"], [5], ["temporary_preference", "negation"]),
            ("zh_to_en", "把E杯两样配料都加倍的建议，已经同意、调好并品尝了吗？", [[4]], "未接受，未调成两杯，也没有品尝结果。", ["加倍批次已完成", "已有品尝结论"], [7], ["execution_status", "numeric"]),
            ("zh_to_en", "无提示的解释说明了哪两个分数相加，不说明什么能力？", [[6]], "说明1/2加1/2是一整杯；没有涉及异分母或配方加倍。", ["独立掌握所有分数运算"], [5], ["support_dependence", "numeric", "claim_limit"]),
            ("en_to_zh", "Did the accepted same-yogurt comparison already produce a new taste observation?", [[7]], "No; ingredients have not been measured again for that comparison.", ["accepted arrangement already executed"], [4], ["execution_status", "negation"]),
            ("en_to_zh", "How did the learner reach the three-quarters versus one-quarter answer?", [[5]], "The teacher first provided a four-part shaded diagram; the answer was scaffolded by that picture.", ["unprompted comparison of all denominators"], [6], ["support_dependence", "numeric"]),
            ("mixed_single", "E杯 correction 是交换了 ingredient labels，还是 made a new mixture？", [[9]], "更正了两配料对应的份量，不是重做。", ["重做实验", "改总量为两杯"], [1], ["correction", "entity", "negation"]),
            ("mixed_single", "Explain the stated ratio goal：学习者已经给出 checked explanation 了吗？", [[10]], "想解释两配料同时加倍为何可保持比例；尚未给出核对过的解释。", ["已完成比例理解目标"], [4], ["goal_status", "negation"]),
            ("mixed_multi", "Compare E and F ingredient amounts，每杯 total volume 相同吗？", [[1, 2], [9, 2]], "E为3/4酸奶+1/4水；F为1/2酸奶+1/2水；总量均1杯。", ["两杯成分比例相同", "体积等于质量"], [3], ["multiple_evidence", "numeric", "entity"]),
            ("mixed_multi", "哪项 fraction answer 有图示帮助，哪项 whole-cup addition 是独立解释？", [[5, 6]], "3/4与1/4比较在图示后；1/2+1/2=1杯为无提示解释。", ["两者均无提示", "推出异分母能力"], [1], ["multiple_evidence", "support_dependence"]),
            ("negative_zh", "E杯实际含糖多少克？", [], None, ["编造糖质量"], [1, 2], ["absent_measurement"]),
            ("negative_en", "Which school does the learner attend?", [], None, ["invented school"], [8], ["absent_profile"]),
            ("negative_mixed", "What was the oven temperature 烘烤纸桥时？", [], None, ["编造烘烤步骤与温度"], [4], ["unrelated_topic"]),
            ("negative_mixed", "配方里用了多少 grams of honey，来自哪个品牌？", [], None, ["虚构蜂蜜用量或品牌"], [1, 2], ["absent_ingredient", "numeric"]),
        ],
    },
    {
        "id": "reading", "topic": "阅读证据与推断", "goal": "区分故事原句、读者推断和解释过程。",
        "sources": [
            ("zh", "observation", "reported_observation", None, "unknown", "学习者摘录故事中的一句：小岚把湿雨衣挂在门边，钟指向四点。摘录没有写她是否淋了雨，也没有写四点是上午还是下午；这里只核对了这一句。"),
            ("en", "observation", "reported_observation", None, "unknown", "In a different scene, Arun put a dry umbrella under his desk at six o'clock. The learner copied that sentence accurately. The sentence did not say that he had walked home, and it did not identify morning or evening."),
            ("zh", "learner_statement", "condition_limit", None, "unknown", "雨衣是湿的可以作为一种推断线索，但故事摘句没有说明它为何湿。不能只靠这句认定小岚刚在雨里走过；别人的解释与原文直接陈述必须分开。"),
            ("en", "plan", "unaccepted_plan", False, "unknown", "The teacher suggested checking the next paragraph for an explicit explanation of the wet coat. The learner has not accepted or carried out this follow-up. No later paragraph has been quoted, so the proposed search supplies no new story fact."),
            ("zh", "learner_answer", "task_specific_response", None, "hinted", "老师先问“哪几个字直接写了下雨”，学习者随后撤回了“原句明确说下雨”的说法。改口发生在提示之后，不代表他在第一次阅读时就独立区分了所有推断。"),
            ("en", "learner_answer", "task_specific_response", None, "independent", "Before a hint about Arun's scene, the learner said that an umbrella being dry does not tell us why it was dry. This was an independent distinction in that scene, not evidence of explaining the wet coat in the other scene."),
            ("zh", "plan", "accepted_plan", True, "unknown", "学习者同意下次先把“原文写明”和“我的猜测”分成两栏再读新故事。新故事尚未发下，分栏练习还没做；这份同意不是新的阅读表现记录。"),
            ("en", "learner_statement", "temporary_request", None, "unknown", "For this passage only, the learner asked for the sentence to be read aloud once before discussing it. They did not request audio for all future passages or state a permanent difficulty with silent reading."),
            ("zh", "correction", "explicit_correction", None, "unknown", "更正摘录中的时间：小岚那句钟指向四点，不是六点，湿雨衣仍挂在门边。六点属于另一人物的场景；两个场景都没有标明上午还是下午。"),
            ("en", "learner_statement", "stated_goal", None, "unknown", "The learner wants to support an inference with words from a story while saying what remains uncertain. That goal has been stated, but no newly checked paragraph analysis shows that it has been achieved."),
        ],
        "queries": [
            ("same_zh", "小岚把什么放在哪里，句中钟指几点？", [[1], [9]], "湿雨衣挂门边，钟指四点；未说明上午下午。", ["小岚场景是六点", "已知淋雨原因"], [2], ["entity", "numeric", "claim_limit"]),
            ("same_zh", "雨衣湿这一句能直接证明她刚在雨中走过吗？", [[3]], "不能，湿的原因未说明，是推断而非原文直接事实。", ["原文明确下雨", "推断等于直接证据"], [1], ["negation", "inference_limit"]),
            ("same_en", "What was Arun's umbrella like, where was it put, and did the line say he walked home?", [[2]], "It was dry, under his desk; the line did not say he walked home.", ["wet coat attributed to Arun", "explicit walk home"], [1], ["entity", "negation"]),
            ("same_en", "Does the read-aloud request establish a lasting difficulty with silent reading?", [[8]], "No, it was one read-aloud request for this passage only.", ["permanent reading difficulty"], [10], ["temporary_preference", "negation"]),
            ("zh_to_en", "继续找下一段解释湿雨衣的建议已经落实、有新引文了吗？", [[4]], "尚未接受和执行，没有后文新引文。", ["后文已证实原因", "已完成查找"], [7], ["execution_status", "negation"]),
            ("zh_to_en", "学习者对Arun的干伞独立指出了什么证据限制？", [[6]], "伞干不能说明为什么干，且不等于解释另一个湿雨衣场景。", ["独立解释湿雨衣原因"], [5], ["support_dependence", "entity", "claim_limit"]),
            ("en_to_zh", "Has the agreed two-column story exercise been completed?", [[7]], "No; the new story has not been handed out and the exercise has not been done.", ["consent equals demonstrated performance"], [4], ["execution_status", "negation"]),
            ("en_to_zh", "Did the learner withdraw the explicit-rain claim before any teacher prompt?", [[5]], "No, it was withdrawn after the teacher asked which words directly said it rained.", ["independent first-reading distinction"], [6], ["support_dependence", "negation"]),
            ("mixed_single", "时间 correction 把小岚那句的 six o'clock 改成几点，还能推出 morning or afternoon 吗？", [[9]], "小岚应是四点；六点是另一人物场景，均未标明上午下午。", ["小岚六点", "四点是已知下午"], [1, 2], ["correction", "entity", "numeric"]),
            ("mixed_single", "这个 inference goal 要同时做到什么，已经 checked as achieved 吗？", [[10]], "用原词支持推断并说明不确定之处；尚无新核对分析证明达成。", ["已达成阅读目标"], [3], ["goal_status", "claim_limit"]),
            ("mixed_multi", "Compare the two quoted scenes：小岚与Arun的物品状态、地点和 clock time？", [[1, 2], [9, 2]], "小岚湿雨衣在门边、四点；Arun干伞在桌下、六点；均未知上午下午。", ["跨人物混淆物品或时间", "断言上午下午"], [3], ["multiple_evidence", "entity", "numeric"]),
            ("mixed_multi", "Wet-coat claim 的撤回与 dry-umbrella uncertainty 哪个有提示、哪个 independent？", [[5, 6]], "撤回原文明说下雨在提示后；干伞原因未知是在提示前指出。", ["两项支持程度相同"], [3], ["multiple_evidence", "support_dependence"]),
            ("negative_zh", "这两段故事的作者全名是什么？", [], None, ["编造作者"], [1, 2], ["absent_entity"]),
            ("negative_en", "Which language is always spoken in the learner's home?", [], None, ["inferred home language"], [8], ["absent_profile"]),
            ("negative_mixed", "Arun的 umbrella price 是多少元？", [], None, ["编造雨伞价格"], [2], ["absent_measurement", "numeric"]),
            ("negative_mixed", "上周 coding assignment 的运行时间是多少 milliseconds？", [], None, ["用故事钟点作程序耗时"], [1, 2], ["unrelated_topic"]),
        ],
    },
    {
        "id": "loops", "topic": "循环程序调试", "goal": "根据执行记录解释程序行为和检查范围。",
        "sources": [
            ("zh", "observation", "reported_observation", None, "unknown", "学习者报告程序G运行三次循环体，每次把初值零的计数器加二，最后显示六。日志只保存了这一次运行，没有记录耗时，也没有证明其他初值下都正确。"),
            ("en", "observation", "reported_observation", None, "unknown", "Program H started its counter at one, added two on each of three loop-body executions, and displayed seven. The learner ran it once. It was not a run with the same starting value as program G, and no timing measurement was saved."),
            ("zh", "learner_statement", "condition_limit", None, "unknown", "G与H虽然都执行三次加二，初值却分别是零和一。不能仅凭最后显示不同就断言某个循环体写错；必须把初值条件也纳入解释。"),
            ("en", "plan", "unaccepted_plan", False, "unknown", "A proposed check would run program G with a negative starting counter and inspect every intermediate value. The learner has not accepted that check, it has not been run, and there is no trace from a negative starting value."),
            ("zh", "learner_answer", "task_specific_response", None, "hinted", "老师提醒“循环之前的数在哪里设置”之后，学习者才找到H的初值为一。这个定位带有提示，不能记作已经独立排查所有循环错误。"),
            ("en", "learner_answer", "task_specific_response", None, "independent", "Before any hint, the learner suggested writing the counter value after each loop-body execution for program G. This was an independent debugging suggestion, but the intermediate-value log was not actually produced during that conversation."),
            ("zh", "plan", "accepted_plan", True, "unknown", "学习者接受下一步让G和H都从零开始，再逐次记下三个中间值的方案。代码尚未改好，也未运行；目前没有这次统一初值后的新输出。"),
            ("en", "learner_statement", "temporary_request", None, "unknown", "For this debugging task the learner asked to see the counter changes in a table beside the code. They did not ask to replace code with tables in every future programming lesson, and no permanent display preference was set."),
            ("zh", "correction", "explicit_correction", None, "unknown", "更正G的运行结果：最后显示六，不是把循环次数三误记成输出。它从零开始，每次加二，循环体执行三次；这次更正没有新增运行记录。"),
            ("en", "learner_statement", "stated_goal", None, "unknown", "The learner wants to explain how the starting value and number of iterations jointly affect a counter. This is a stated goal; no checked explanation covering changed iteration counts has yet been given."),
        ],
        "queries": [
            ("same_zh", "G从多少开始、循环体执行几次、最后显示多少？", [[1], [9]], "从0开始，执行3次每次加2，最后6。", ["输出3", "已经证明所有初值正确"], [2], ["numeric", "entity"]),
            ("same_zh", "两个最后数不同足以判定某个循环体错误吗？", [[3]], "不够，初值0与1不同必须考虑。", ["只凭末值定位循环错误"], [1, 2], ["condition", "negation"]),
            ("same_en", "What starting value and final output were logged for H, and how many runs were saved?", [[2]], "Start 1, output 7, one run.", ["H began at zero", "multiple verified runs"], [1], ["numeric", "entity", "repetition"]),
            ("same_en", "Was the requested table made a permanent replacement for code?", [[8]], "No, it was requested beside the code for this task.", ["permanent no-code preference"], [10], ["temporary_preference", "negation"]),
            ("zh_to_en", "负数初值的检查已经接受并留下逐次日志了吗？", [[4]], "没有接受，也未运行，没有负数初值轨迹。", ["已验证负数情况", "已有逐次日志"], [7], ["execution_status", "negation"]),
            ("zh_to_en", "学习者无提示建议了哪种调试记录，它真的写出来了吗？", [[6]], "建议每次循环体后记计数器；对话中并未实际生成该日志。", ["建议即已执行", "有完整中间值证据"], [5], ["support_dependence", "execution_status"]),
            ("en_to_zh", "Did the agreed common-zero rerun already establish new outputs?", [[7]], "No; the code changes and rerun have not happened.", ["accepted rerun completed", "invented new outputs"], [4], ["execution_status", "condition"]),
            ("en_to_zh", "How much prompting preceded finding H's initial value?", [[5]], "A teacher reminder about where the pre-loop value is set came first.", ["independent identification of all loop errors"], [6], ["support_dependence"]),
            ("mixed_single", "G的 correction 区分了 iteration count 和 final output 中哪两个数？", [[9]], "循环次数为3，最终输出为6，不是新增运行。", ["循环6次", "更正是一条新运行"], [1], ["correction", "numeric"]),
            ("mixed_single", "关于 initial value 与 iterations 的 goal，是否已有 changed-count explanation？", [[10]], "想解释两者共同作用；未给出涵盖变化次数的核对解释。", ["已完成通用循环解释"], [3], ["goal_status", "negation"]),
            ("mixed_multi", "Compare G and H：各自 starting value、三次加二后的 output 是什么？", [[1, 2], [9, 2]], "G从0到6，H从1到7，都是3次加2。", ["把G与H初值对调", "不同末值证明错误"], [3], ["multiple_evidence", "numeric", "entity"]),
            ("mixed_multi", "找到H初值和提出 trace table，哪项 prompted，哪项 independent but not executed？", [[5, 6]], "定位H初值在提示后；提出逐次记录无提示但日志未做。", ["独立建议等于已执行记录", "H初值无提示定位"], [3], ["multiple_evidence", "support_dependence", "execution_status"]),
            ("negative_zh", "这次运行耗时多少毫秒？", [], None, ["把计数器结果当耗时"], [1, 2], ["absent_measurement", "numeric"]),
            ("negative_en", "Which exact programming language version ran these programs?", [], None, ["invented language or version"], [1, 2], ["absent_instrument"]),
            ("negative_mixed", "Learner 的 class number 和年级分别是什么？", [], None, ["编造班级年级"], [8], ["absent_profile"]),
            ("negative_mixed", "昨天的 swimming lesson 总共游了多少 meters？", [], None, ["用循环计数作游泳距离"], [1, 2], ["unrelated_topic"]),
        ],
    },
    {
        "id": "evaporation", "topic": "蒸发与测量", "goal": "把液面变化的观察与条件和测量限制联系起来。",
        "sources": [
            ("zh", "observation", "reported_observation", None, "unknown", "学习者在上午九点记录浅碟J有五十毫升水，下午三点读到四十二毫升，放在通风的窗边。只记录这一日，没有称质量，也没有收集减少部分的去向。"),
            ("en", "observation", "reported_observation", None, "unknown", "Deep cup K was read as 50 mL at nine in the morning and 47 mL at three in the afternoon. It stood in a cupboard. The learner recorded just this day; airflow and vessel shape were not matched to dish J."),
            ("zh", "learner_statement", "condition_limit", None, "unknown", "浅碟J与深杯K的位置和容器形状都不同，没有测量空气流速或水温。因此液面变化可以报告，却不能仅凭这一日把差异全部归因于开口面积。"),
            ("en", "plan", "unaccepted_plan", False, "unknown", "A suggested next comparison would use identical dishes in two airflow conditions and read their volumes at fixed times. The learner has not agreed to this suggestion, no dishes have been set up for it, and it has produced no measurements."),
            ("zh", "learner_answer", "task_specific_response", None, "hinted", "老师先问“如果要比较减少多少，还缺哪个时刻的数”，学习者才指出应保留起始读数。这是在提示后说明计算所需信息，不是独立完成所有测量设计。"),
            ("en", "learner_answer", "task_specific_response", None, "independent", "Without a hint, the learner suggested reading the bottom of the meniscus at eye level when using a measuring cylinder. This was a measurement suggestion, not a report that all previous readings had already been taken that way."),
            ("zh", "plan", "accepted_plan", True, "unknown", "学习者同意明天把两个相同浅碟并排放好，每隔两小时记录体积。明天的安排还没开始，不能把这个间隔或同型容器写成今天已做到的条件。"),
            ("en", "learner_statement", "temporary_request", None, "unknown", "For today's measurement discussion, the learner asked for the volume numbers to be shown in large print. No request was made to use large print in every lesson, and no visual condition or permanent accessibility need was reported."),
            ("zh", "correction", "explicit_correction", None, "unknown", "更正J碟下午三点的体积为四十二毫升，不是四十二克；上午九点为五十毫升。改的是单位误记，没有新增称重证据，也没有第二日的数据。"),
            ("en", "learner_statement", "stated_goal", None, "unknown", "The learner wants to explain where the water that is no longer in an open vessel might go and what evidence would support that explanation. No collection or identification of water vapor has yet been reported as a completed investigation."),
        ],
        "queries": [
            ("same_zh", "J碟九点和十五点的读数分别是多少，单位是什么？", [[1], [9]], "50毫升与42毫升，都是体积而非质量。", ["42克", "有称重证据"], [2], ["numeric", "unit", "entity"]),
            ("same_zh", "一天的记录能把减少量差异全部解释为开口面积吗？", [[3]], "不能，位置和形状都不同，流速及水温未测。", ["已控制温度和气流", "单独面积因果已证实"], [1, 2], ["condition", "causality"]),
            ("same_en", "What readings and location were recorded for K, and were its conditions matched to J?", [[2]], "50 then 47 mL in a cupboard; airflow and shape were not matched.", ["42 mL for K", "matched airflow"], [1], ["numeric", "entity", "condition"]),
            ("same_en", "Does the large-print request establish a permanent visual accessibility need?", [[8]], "No, it was a present discussion request without a reported visual condition.", ["diagnosed visual need", "permanent preference"], [10], ["temporary_preference", "negation"]),
            ("zh_to_en", "同型浅碟、不同气流的建议，已有同意和测量结果了吗？", [[4]], "未接受、未布置，也无测量结果。", ["建议已执行", "已得到气流效应"], [7], ["execution_status", "negation"]),
            ("zh_to_en", "学习者独立提出怎么读量筒，这能证明先前每次都这么读吗？", [[6]], "视线平齐读凹液面底部；不能证明旧读数均按此取得。", ["建议倒推成已执行操作"], [5], ["support_dependence", "claim_limit"]),
            ("en_to_zh", "Was the accepted every-two-hours identical-dish schedule already used today?", [[7]], "No; it is tomorrow's unstarted arrangement, not today's conditions.", ["future conditions retroactively applied to today"], [4], ["execution_status", "numeric", "condition"]),
            ("en_to_zh", "Did the learner identify the need for an initial reading without a prompt?", [[5]], "No; the teacher first asked which time point was missing.", ["independent complete measurement design"], [6], ["support_dependence", "negation"]),
            ("mixed_single", "J的 unit correction 是从 grams 改成什么，增加了 weighing evidence 吗？", [[9]], "42克更正为42毫升，没有新增称重证据。", ["体积等于质量", "已称重"], [1], ["correction", "unit", "negation"]),
            ("mixed_single", "Where the missing water goes 这个 goal，已有 completed vapor collection 吗？", [[10]], "想解释水的去向及支持证据，尚未报告完成水蒸气收集或鉴定。", ["已完成蒸气鉴定"], [3], ["goal_status", "execution_status"]),
            ("mixed_multi", "Compare J and K volume changes from 09:00 to 15:00：各减少多少 millilitres？", [[1, 2], [9, 2]], "J由50至42，减少8毫升；K由50至47，减少3毫升。", ["把8和3视作克数", "不同时段比较"], [3], ["multiple_evidence", "numeric", "unit"]),
            ("mixed_multi", "保留 initial reading 与 eye-level meniscus 两条建议，哪条 prompted，哪条 independent？", [[5, 6]], "保留起始读数在提示后；平视凹液面底部的建议无提示。", ["建议即证实全部旧测量合规"], [3], ["multiple_evidence", "support_dependence"]),
            ("negative_zh", "当天窗边的空气流速准确是多少？", [], None, ["编造空气流速"], [3], ["absent_measurement"]),
            ("negative_en", "What is the learner's permanent preferred interface language?", [], None, ["infer permanent language from a print request"], [8], ["absent_profile"]),
            ("negative_mixed", "What was the mass of dissolved salt 以及化学纯度？", [], None, ["编造溶质或纯度"], [1, 2], ["absent_ingredient"]),
            ("negative_mixed", "这周 music rehearsal 每首曲目的 tempo 是多少 BPM？", [], None, ["用两小时观察间隔当音乐速度"], [7], ["unrelated_topic"]),
        ],
    },
]

NEGATIVE_GAPS = {
    "germination": ["没有科学考试记录或成绩。", "只描述盘的摆放，没有光的波长测量。", "没有家庭住址、城市或街道信息。", "没有昨天烹饪试验的盐质量记录；浇水体积不支持盐质量。"],
    "paper_bridge": ["没有纸张每平方米克重测量或规格。", "没有出生月份信息。", "没有称量仪器品牌或校准记录，来源只记垫圈枚数。", "没有昨天词汇测验成绩；垫圈数量不是测验得分。"],
    "fractions": ["配方体积不含糖质量，且没有含糖量测量。", "没有就读学校信息。", "没有纸桥烘烤步骤或烤箱温度记录。", "没有蜂蜜配料、质量或品牌信息。"],
    "reading": ["摘句未提供故事作者姓名。", "没有家庭常用语言或永久语言设置。", "Arun的摘句没有雨伞价格。", "没有上周程序作业计时记录；故事钟点不是运行耗时。"],
    "loops": ["记录明确没有计时数据；计数器输出不提供毫秒耗时。", "程序行为记录没有注明语言名称或版本。", "没有学习者班级或年级信息。", "没有游泳课或游泳距离记录。"],
    "evaporation": ["记录明确未测空气流速，不能由液体体积反推准确流速。", "大字请求未给出永久界面语言偏好。", "没有溶解盐的质量、加入记录或纯度信息。", "没有音乐排练曲目或速度信息；观察间隔不能充当BPM。"],
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as target:
        target.write(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n")


def write_jsonl(path, rows):
    with path.open("x", encoding="utf-8", newline="\n") as target:
        for row in rows:
            target.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")


def record_schemas():
    """Publish the four concrete record shapes; not a schema execution engine."""
    text = {"type": "string", "minLength": 1}
    identity = {"type": "string", "pattern": "^[a-z0-9_]+$"}
    def obj(properties):
        return {"type": "object", "additionalProperties": False,
                "required": sorted(properties), "properties": properties}
    def array(item, minimum=0, maximum=None, unique=False):
        value = {"type": "array", "items": item, "minItems": minimum}
        if maximum is not None:
            value["maxItems"] = maximum
        if unique:
            value["uniqueItems"] = True
        return value
    context = {key: text for key in ("learner_id", "namespace", "activity_id", "scope", "session_id")}
    source = obj({
        "id": identity, "pack_id": {"enum": [pack["id"] for pack in PACKS]},
        "text": text, "language": {"enum": ["zh", "en"]},
        **context,
        "event_kind": {"enum": ["observation", "learner_statement", "plan", "learner_answer", "correction"]},
        "source_type": {"const": "simulated"}, "version": {"type": "integer", "minimum": 1},
        "status": {"enum": ["active", "invalidated"]}, "accepted": {"type": ["boolean", "null"]},
        "support_level": {"enum": ["unknown", "hinted", "independent"]},
        "claim_type": {"enum": ["reported_observation", "condition_limit", "unaccepted_plan",
                               "accepted_plan", "task_specific_response", "temporary_request",
                               "explicit_correction", "stated_goal"]},
        "hardware_verified": {"const": False},
    })
    citation = obj({"source_id": identity, "version": {"type": "integer", "minimum": 1},
                    "start_char": {"type": "integer", "minimum": 0},
                    "end_char": {"type": "integer", "minimum": 1}, "quote": text})
    claim = obj({"claim": text, "citations": array({"$ref": "#/$defs/citation"}, 1, 6)})
    query = obj({
        "id": identity, "pack_id": {"enum": [pack["id"] for pack in PACKS]},
        "text": text, "language": {"enum": ["zh", "en", "mixed"]},
        "mode": {"enum": ["same_zh", "same_en", "zh_to_en", "en_to_zh", "mixed_single",
                         "mixed_multi", "negative_zh", "negative_en", "negative_mixed"]},
        "goal": text, "context": {"$ref": "#/$defs/context"},
        "candidate_pool_ids": array(identity, 60, 60, True), "tags": array(text, 1, unique=True),
    })
    label = obj({
        "query_id": identity, "answerable": {"type": "boolean"},
        "sufficient_evidence_sets": array(array(identity, 1, 3, True), maximum=3, unique=True),
        "claim_checks": array({"$ref": "#/$defs/claim_check"}, maximum=6),
        "forbidden_claims": array(text, 1, unique=True), "context_only_ids": array(identity, unique=True),
        "distractor_reason": text, "no_evidence_reason": {"type": ["string", "null"]},
        "annotation_origin": {"const": "method-aware_agent_authored_not_blind"},
    })
    control = obj({
        "id": identity, "source": {"$ref": "#/$defs/source"},
        "context": obj({**context, "expected_source_version": {"type": "integer", "minimum": 1}}),
        "reference_source_id": identity,
        "control_kind": {"enum": ["foreign_learner", "foreign_namespace", "wrong_activity",
                                 "invalidated_source", "version_mismatch", "same_text_other_owner"]},
        "expected_eligible": {"const": False}, "same_text_as_reference": {"const": True},
    })
    return {"$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": VERSION + " record schemas",
            "$comment": "One JSONL record at a time. Cross-record links, scope, citation spans and counts use check_diagnostic_postfreeze.py; no automatic entailment claim.",
            "oneOf": [{"$ref": "#/$defs/" + key} for key in ("source", "query", "label", "scope_control")],
            "$defs": {"source": source, "context": obj(context), "query": query,
                      "citation": citation, "claim_check": claim, "label": label, "scope_control": control}}


def compile_records():
    sources, queries, labels = [], [], []
    for pack in PACKS:
        pid = pack["id"]
        for number, spec in enumerate(pack["sources"], 1):
            language, kind, claim, accepted, support, text = spec
            sources.append({"id": f"{pid}_s{number:02}", "pack_id": pid, "text": text,
                            "language": language, **OWNER, "session_id": f"synthetic_{pid}",
                            "event_kind": kind, "source_type": "simulated", "version": 1,
                            "status": "active", "accepted": accepted, "support_level": support,
                            "claim_type": claim, "hardware_verified": False})
    source_map = {row["id"]: row for row in sources}
    pool = [row["id"] for row in sources]
    for pack in PACKS:
        pid = pack["id"]
        for number, spec in enumerate(pack["queries"], 1):
            mode, text, alternatives, claim, forbidden, context, tags = spec
            qid = f"{pid}_q{number:02}"
            sets = [[f"{pid}_s{n:02}" for n in option] for option in alternatives]
            required = sorted({sid for option in sets for sid in option})
            language = "mixed" if mode.startswith("mixed") or mode == "negative_mixed" else (
                "zh" if mode in ("same_zh", "zh_to_en", "negative_zh") else "en")
            queries.append({"id": qid, "pack_id": pid, "text": text, "language": language,
                            "mode": mode, "goal": pack["goal"], "context": {**OWNER, "session_id": f"synthetic_{pid}"},
                            "candidate_pool_ids": pool, "tags": tags})
            # Full-source spans deliberately include all limiting conditions.
            # This is exact citation validation, not automatic entailment grading.
            citations = [{"source_id": sid, "version": 1, "start_char": 0,
                          "end_char": len(source_map[sid]["text"]), "quote": source_map[sid]["text"]}
                         for sid in required]
            contexts = [f"{pid}_s{n:02}" for n in context if f"{pid}_s{n:02}" not in required]
            labels.append({"query_id": qid, "answerable": bool(sets), "sufficient_evidence_sets": sets,
                           "claim_checks": [{"claim": claim, "citations": citations}] if sets else [],
                           "forbidden_claims": forbidden, "context_only_ids": contexts,
                           "distractor_reason": "同主题或共享表面词，但缺少此问题要求的实体、条件、执行状态或测量；不计作完整答案证据。",
                           "no_evidence_reason": None if sets else NEGATIVE_GAPS[pid][number - 13],
                           "annotation_origin": "method-aware_agent_authored_not_blind"})
    controls = []
    cases = ["foreign_learner", "foreign_namespace", "wrong_activity", "invalidated_source", "version_mismatch", "same_text_other_owner"] * 2
    for index, reason in enumerate(cases):
        base = sources[(index * 5) % len(sources)]
        altered = dict(base, id=f"scope_source_{index+1:02}")
        context = {**OWNER, "session_id": base["session_id"], "expected_source_version": 1}
        if reason in ("foreign_learner", "same_text_other_owner"):
            altered["learner_id"] = "different_synthetic_learner"
        elif reason == "foreign_namespace":
            altered["namespace"] = "real"
        elif reason == "wrong_activity":
            altered["activity_id"] = "different_logical_workshop"
        elif reason == "invalidated_source":
            altered["status"] = "invalidated"
        else:
            altered["version"] = 2
        controls.append({"id": f"scope_{index+1:02}", "source": altered, "context": context,
                         "reference_source_id": base["id"], "control_kind": reason,
                         "expected_eligible": False, "same_text_as_reference": True})
    return sources, queries, labels, controls


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root, output = args.root.resolve(), args.output.resolve()
    spec = root / "docs/MEMORY_CHALLENGE_SPEC_20261104.md"
    method = root / "rust/ai-tutor/tests/fixtures/lexical_shadow_method_v1.json"
    if digest(spec) != SPEC_SHA or digest(method) != METHOD_SHA:
        raise ValueError("frozen_spec_or_method_changed")
    output.mkdir(parents=True, exist_ok=False)
    sources, queries, labels, controls = compile_records()
    for name, rows in [("sources.jsonl", sources), ("queries.jsonl", queries),
                       ("labels.jsonl", labels), ("scope_controls.jsonl", controls)]:
        write_jsonl(output / name, rows)
    write_json(output / "record_schemas.json", record_schemas())
    manifest = {
        "fixture_version": VERSION, "synthetic_only": True,
        "authorship": "agent authored after observing frozen methods and v2; not human, blind, independent random sampling or real learner data",
        "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "method_sha256": METHOD_SHA, "spec_sha256": SPEC_SHA,
        "authoring_source_sha256": digest(Path(__file__)),
        "record_schema": {"file": "record_schemas.json", "sha256": digest(output / "record_schemas.json")},
        "files": {name: {"sha256": digest(output / name), "records": count} for name, count in
                  [("sources.jsonl", 60), ("queries.jsonl", 96), ("labels.jsonl", 96), ("scope_controls.jsonl", 12)]},
        "packs": [{"id": pack["id"], "topic": pack["topic"], "sources": 10, "positive_queries": 12, "negative_queries": 4} for pack in PACKS],
        "expected_modes": {"same_zh": 12, "same_en": 12, "zh_to_en": 12, "en_to_zh": 12,
                           "mixed_single": 12, "mixed_multi": 12, "negative_zh": 6, "negative_en": 6, "negative_mixed": 12},
        "expected_languages": {"zh": 30, "en": 30, "mixed": 36},
        "evaluation": {"top_k": 5, "content_budget_chars": 2400,
                       "conditions": ["current_query", "query_plus_goal"], "goal_query_format": "{text} {goal} ",
                       "complete_evidence_at_5_minimum": 65, "single_cross_language_at_5_minimum": 20,
                       "multi_evidence_at_5_minimum": 10, "source_control_leaks_allowed": 0,
                       "per_topic_complete_evidence_regression_allowed": 1,
                       "overall_complete_evidence_regression_allowed": 0},
        "scope": "offline logical workshop, not production modular_rover mapping or Store integration proof",
        "citation_span_unit": "Unicode code points; end exclusive",
        "model_called": False, "retrieval_run": False, "production_default_changed": False,
        "limitations": ["Full-source citations include limitations; checker verifies text spans, not semantic entailment",
                        "Alternative evidence sets can refer to a report and its correction; repetition never raises independent credibility",
                        "All 60 eligible sources share the logical workshop; topics are not used to prefilter candidates",
                        "No retrieval scores were consulted to alter questions or labels", "Static checks do not establish educational usefulness"],
    }
    write_json(output / "manifest.json", manifest)
    print(json.dumps({"fixture": str(output), "sources": len(sources), "queries": len(queries),
                      "controls": len(controls), "manifest_sha256": digest(output / "manifest.json"),
                      "model_called": False, "retrieval_run": False}))


if __name__ == "__main__":
    main()
